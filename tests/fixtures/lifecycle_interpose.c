/* Linux test-only syscall gates. Real raise/kill are forwarded; no model output. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <time.h>
#include <pthread.h>
#include <stdatomic.h>
#include <sys/wait.h>
static ssize_t (*real_write)(int, const void *, size_t);
static pid_t owner_pid;
static int fork_calls;
static pid_t launcher_pid, anchor_pid;
static int anchor_probes, wait_faulted, owned_forks;
static int armed, delivered, selected_signal, boundary, restore_fault, register_fault;
static int group_mode, int_restored, inflight, reset_race;
static atomic_int retiring, attempting;
static atomic_int entered, release_entry;
static pthread_t raiser;
static void (*product[2])(int);
static int (*real_action)(int, const struct sigaction *, struct sigaction *);
static int (*real_kill)(pid_t, int);
static pid_t (*next_fork)(void);
static int (*next_group)(pid_t, pid_t);
static void event(const char *kind, int value) {
    const char *path = getenv("LIFECYCLE_EVENTS");
    if (!path) _exit(120);
    int fd = open(path, O_CREAT | O_WRONLY | O_APPEND, 0600);
    if (fd < 0) _exit(121);
    char line[180];
    int n = snprintf(line, sizeof(line), "%s value=%d int_restored=%d\n",
                     kind, value, int_restored);
    if (write(fd, line, n) != n) _exit(122);
    close(fd);
}
void lifecycle_arm(int signal, int at, int restoration, int registration, int group) {
    real_action = dlsym(RTLD_NEXT, "sigaction");
    real_kill = dlsym(RTLD_NEXT, "kill");
    next_fork = dlsym(RTLD_NEXT, "fork");
    next_group = dlsym(RTLD_NEXT, "setpgid");
    if (!real_action || !real_kill) _exit(123);
    real_write = dlsym(RTLD_NEXT, "write");
    if (!real_write) _exit(138);
    owner_pid = getpid(); fork_calls = 0;
    armed = 1; selected_signal = signal; inflight = at == 2; reset_race = at == 3; boundary = (inflight || reset_race) ? 0 : at;
    restore_fault = restoration; register_fault = registration; group_mode = group;
}
int lifecycle_retiring(void) { return atomic_load_explicit(&retiring, memory_order_seq_cst); }
int lifecycle_attempting_install(void) { atomic_store_explicit(&attempting, 1, memory_order_seq_cst); return 1; }
void lifecycle_observed_entry(int signal) {
    if (!inflight || signal != selected_signal) return;
    atomic_store_explicit(&entered, 1, memory_order_seq_cst);
    struct timespec pause = {0, 1000000};
    while (!atomic_load_explicit(&release_entry, memory_order_seq_cst)) nanosleep(&pause, NULL);
}
static void *raise_thread(void *unused) {
    (void)unused;
    sigset_t unblock; sigemptyset(&unblock); sigaddset(&unblock, selected_signal);
    if (pthread_sigmask(SIG_UNBLOCK, &unblock, NULL)) _exit(129);
    int rc = raise(selected_signal); event("inflight_real_raise_return", rc);
    if (rc) _exit(130);
    return NULL;
}
void lifecycle_finish_inflight(void) {
    atomic_store_explicit(&release_entry, 1, memory_order_seq_cst);
    if (pthread_join(raiser, NULL)) _exit(131);
    event("inflight_joined", selected_signal);
}
int sigaction(int sig, const struct sigaction *act, struct sigaction *old) {
    if (!real_action) real_action = dlsym(RTLD_NEXT, "sigaction");
    if (!real_action) _exit(123);
    int index = sig == SIGINT ? 0 : sig == SIGTERM ? 1 : -1;
    if (!armed || getpid() != owner_pid || index < 0 || !act) return real_action(sig, act, old);
    if (old) {
        if (sig == SIGTERM && register_fault) {
            register_fault = 0; event("registration_fault", sig); errno = EIO; return -1;
        }
        int rc = real_action(sig, act, old);
        if (!rc) { product[index] = act->sa_handler; event("installed", sig); }
        return rc;
    }
    struct sigaction current;
    if (real_action(sig, NULL, &current)) _exit(124);
    if (product[index] && current.sa_handler == product[index]) {
        if (!delivered && selected_signal && sig == (boundary ? SIGTERM : SIGINT)) {
            struct sigaction active;
            sigset_t blocked;
            int selected = selected_signal == SIGINT ? 0 : 1;
            if (real_action(selected_signal, NULL, &active) ||
                active.sa_handler != product[selected] ||
                sigprocmask(SIG_BLOCK, NULL, &blocked) ||
                sigismember(&blocked, selected_signal)) _exit(125);
            delivered = 1; event("verified_owned_unblocked", selected_signal);
            if (inflight) {
                if (pthread_create(&raiser, NULL, raise_thread, NULL)) _exit(132);
                struct timespec pause = {0, 1000000};
                for (int i = 0; !atomic_load_explicit(&entered, memory_order_seq_cst); ++i) {
                    if (i == 2000) _exit(133);
                    nanosleep(&pause, NULL);
                }
                event("actual_handler_publication_inflight", selected_signal);
            }
            int rc = inflight ? 0 : raise(selected_signal);
            event("real_raise_return", rc);
            if (rc) _exit(126);
            if (reset_race) {
                atomic_store_explicit(&retiring, 1, memory_order_seq_cst);
                struct timespec pause = {0, 1000000};
                for (int i = 0; !atomic_load_explicit(&attempting, memory_order_seq_cst); ++i) {
                    if (i == 2000) _exit(134);
                    nanosleep(&pause, NULL);
                }
                event("independent_install_attempted_while_owned", selected_signal);
            }
        }
        if (restore_fault == sig) {
            restore_fault = 0; event("restoration_fault", sig); errno = EIO; return -1;
        }
    }
    int rc = real_action(sig, act, old);
    if (!rc && sig == SIGINT) int_restored = 1;
    return rc;
}
/* Source-derived UNIX03 zombie-only model; not a Darwin ABI/runtime probe. */
static int zombie_only(pid_t group) {
    DIR *dir = opendir("/proc");
    if (!dir) _exit(127);
    int members = 0, live = 0;
    struct dirent *entry;
    while ((entry = readdir(dir))) {
        char *end; long pid = strtol(entry->d_name, &end, 10);
        if (*end || pid <= 0) continue;
        char path[80], stat[4096];
        snprintf(path, sizeof(path), "/proc/%ld/stat", pid);
        FILE *file = fopen(path, "r");
        if (!file) continue;
        char *read = fgets(stat, sizeof(stat), file); fclose(file);
        if (!read) continue;
        char *tail = strrchr(stat, ')'), state; int parent, pgid;
        if (!tail || sscanf(tail + 1, " %c %d %d", &state, &parent, &pgid) != 3) _exit(128);
        if (pgid == group) { members++; if (state != 'Z' && state != 'X') live++; }
    }
    closedir(dir);
    return members && !live;
}
int kill(pid_t pid, int sig) {
    if (!real_kill) real_kill = dlsym(RTLD_NEXT, "kill");
    if (!real_kill) _exit(123);
    if (armed && getpid() == owner_pid && group_mode == 16 && pid == launcher_pid &&
        (sig == SIGTERM || sig == SIGKILL)) {
        event("direct_signal_denied", sig); errno = EPERM; return -1;
    }
    if (armed && getpid() == owner_pid && group_mode >= 14 && pid == -launcher_pid)
        event("group_signal_forwarded", sig);
    if (!armed || !group_mode || pid >= -1) return real_kill(pid, sig);
    if (group_mode == 1 && zombie_only(-pid)) {
        event("proven_zombie_only_EPERM", sig); errno = EPERM; return -1;
    }
    if (group_mode == 13 && sig == SIGKILL) {
        if (real_kill(pid, SIGUSR1)) _exit(139);
        struct timespec pause = {0, 1000000};
        for (int i = 0; i < 100 && !zombie_only(-pid); ++i) nanosleep(&pause, NULL);
        event("helper_boundary_real_SIGUSR1", SIGUSR1);
    }
    int rc = real_kill(pid, sig), saved = errno;
    if (group_mode == 1 && sig == SIGTERM && !rc) {
        /* Only pace observation; preserve real TERM delivery and its result. */
        struct timespec pause = {0, 1000000};
        for (int i = 0; i < 100 && !zombie_only(-pid); ++i) nanosleep(&pause, NULL);
    }
    if (group_mode == 2 && sig == SIGKILL) {
        event("genuine_KILL_EPERM_after_delivery", sig); errno = EPERM; return -1;
    }
    if (group_mode == 3 && sig == SIGTERM) {
        event("genuine_TERM_EPERM_after_delivery", sig); errno = EPERM; return -1;
    }
    errno = saved; return rc;
}

/* Narrow helper-only startup gates: launcher sets (0,0); helper joins (0,PGID). */
pid_t fork(void) {
    if (!next_fork) next_fork = dlsym(RTLD_NEXT, "fork");
    if (!next_fork) _exit(135);
    if (armed && group_mode == 12 && fork_calls++ == 0) { event("launcher_fault_fork", EAGAIN); errno = EAGAIN; return -1; }
    if (armed && group_mode == 5 && fork_calls++ > 0) { event("anchor_fault_fork", EAGAIN); errno = EAGAIN; return -1; }
    pid_t child = next_fork();
    if (armed && getpid() == owner_pid && group_mode >= 14 && child > 0) {
        if (owned_forks++ % 2 == 0) { launcher_pid = child; event("owned_launcher", child); }
        else { anchor_pid = child; event("owned_anchor", child); }
    }
    return child;
}
/* Gates match positively captured child identities, not global syscall counts. */
int waitid(idtype_t type, id_t id, siginfo_t *info, int options) {
    static int (*next)(idtype_t, id_t, siginfo_t *, int);
    if (!next) next = dlsym(RTLD_NEXT, "waitid");
    if (!next) _exit(140);
    if (armed && getpid() == owner_pid && group_mode >= 14 && type == P_PID &&
        id == (id_t)anchor_pid && options == (WEXITED | WNOHANG | WNOWAIT)) {
        if (++anchor_probes == 2 && group_mode == 14) {
            event("delivery_anchor_EIO", id); errno = EIO; return -1;
        }
        event("anchor_wait_forwarded", id);
    }
    return next(type, id, info, options);
}
pid_t waitpid(pid_t pid, int *status, int options) {
    static pid_t (*next)(pid_t, int *, int);
    if (!next) next = dlsym(RTLD_NEXT, "waitpid");
    if (!next) _exit(141);
    if (armed && getpid() == owner_pid && pid == launcher_pid && options == WNOHANG) {
        if (group_mode == 15 && anchor_probes >= 2 && !wait_faulted++) {
            event("delivery_launcher_EIO", pid); errno = EIO; return -1;
        }
        pid_t result = next(pid, status, options); int saved = errno;
        if (result == pid) event("launcher_reaped", pid);
        errno = saved; return result;
    }
    return next(pid, status, options);
}
int setpgid(pid_t pid, pid_t group) {
    if (!next_group) next_group = dlsym(RTLD_NEXT, "setpgid");
    if (!next_group) _exit(136);
    if (armed && group > 0 && (group_mode == 6 || group_mode == 7)) {
        event("anchor_fault_join", group);
        if (group_mode == 7) _exit(137);
        errno = EPERM; return -1;
    }
    return next_group(pid, group);
}

/* Readiness gates apply only to the actual helper's fixed protocol publication. */
ssize_t write(int fd, const void *buffer, size_t count) {
    if (!real_write) real_write = dlsym(RTLD_NEXT, "write");
    if (!real_write) _exit(138);
    if (armed && getpid() != owner_pid && count == 1 && *(const unsigned char *)buffer == 0xA7) {
        if (group_mode == 8) { unsigned char wrong = 0x31; event("anchor_bad_ack", fd); return real_write(fd, &wrong, 1); }
        if (group_mode == 9) { event("anchor_no_ack", fd); struct timespec pause = {0, 300000000}; nanosleep(&pause, NULL); return real_write(fd, buffer, count); }
        if (group_mode == 11) { event("anchor_failed_ack", fd); errno = EPIPE; return -1; }
    }
    return real_write(fd, buffer, count);
}
