// Linux-only observational delegates: never change permissions or syscall results.
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
#include <wchar.h>

// This channel is independent of creation.tsv; the parent rejects either signal.
static void failure(void) {
    static const char marker[] = "FIFO_CREATION_OBSERVER_FAILURE\n";
    size_t offset = 0;
    while (offset < sizeof(marker) - 1) {
        ssize_t bytes = write(STDERR_FILENO, marker + offset,
                              sizeof(marker) - 1 - offset);
        if (bytes < 0 && errno == EINTR) continue;
        if (bytes <= 0) _exit(86);
        offset += (size_t)bytes;
    }
}

static int write_record(int fd, const char *line, size_t length, int retry) {
    size_t offset = 0;
    int interrupted = 0;
    while (offset < length) {
        ssize_t bytes;
        if (retry && !interrupted) {
            interrupted = 1;
            errno = EINTR;
            bytes = -1;
        } else {
            size_t count = length - offset;
            if (retry && count > 7) count = 7;
            bytes = write(fd, line + offset, count);
        }
        if (bytes < 0 && errno == EINTR) continue;
        if (bytes <= 0) return -1;
        offset += (size_t)bytes;
    }
    return 0;
}

static void observe(const char *kind, const char *path, mode_t requested,
                    int result, int directory) {
    int saved = errno;
    const char *log = getenv("FIFO_CREATION_LOG");
    const char *scope = getenv("FIFO_CREATION_SCOPE");
    struct stat info;
    if (!log || !scope || strncmp(path, scope, strlen(scope)) || result != 0) {
        errno = saved;
        return;
    }
    int rc = directory == AT_FDCWD ? lstat(path, &info)
                                  : fstatat(directory, path, &info, AT_SYMLINK_NOFOLLOW);
    // Faults start at the second FIFO, retaining valid directory AND FIFO rows.
    static _Atomic unsigned fifos;
    const char *fault = getenv("FIFO_CREATION_FAULT");
    if (!fault || strcmp(kind, "fifo") ||
        atomic_fetch_add_explicit(&fifos, 1, memory_order_relaxed) == 0) fault = "";
    char line[4096];
    int bytes = snprintf(line, sizeof(line), "%s\t%u\t%d\t%u\t%u\t%lu\t%lu\t%s\n",
                         kind, (unsigned)requested, rc,
                         rc == 0 ? (unsigned)info.st_mode : 0,
                         rc == 0 ? (unsigned)info.st_uid : 0,
                         rc == 0 ? (unsigned long)info.st_dev : 0,
                         rc == 0 ? (unsigned long)info.st_ino : 0, path);
    if (!strcmp(fault, "format"))
        bytes = snprintf(line, sizeof(line), "%lc", (wint_t)0xd800);
    if (!strcmp(fault, "oversized"))
        bytes = snprintf(line, sizeof(line), "%*s", (int)strlen(fault) * 512, path);
    int failed = rc != 0 || bytes <= 0 || (size_t)bytes >= sizeof(line);
    if (!failed) {
        const char *destination = log;
        if (!strcmp(fault, "open")) destination = scope; // Real EISDIR.
        if (!strcmp(fault, "write") || !strcmp(fault, "report"))
            destination = "/dev/full"; // Real ENOSPC.
        int fd = open(destination, O_WRONLY | O_APPEND | O_CREAT | O_CLOEXEC, 0600);
        if (fd < 0) failed = 1;
        else {
            if (!strcmp(fault, "incomplete")) {
                // A real partial record followed by an unwritable descriptor.
                if (write_record(fd, line, 7, 0) != 0) failed = 1;
                if (close(fd) != 0) failed = 1;
                failed |= write_record(fd, line + 7, (size_t)bytes - 7, 0) != 0;
            } else {
                failed |= write_record(fd, line, (size_t)bytes,
                                       !strcmp(fault, "retry")) != 0;
                if (!strcmp(fault, "close") && close(fd) != 0) failed = 1;
                failed |= close(fd) != 0;
            }
        }
    }
    if (failed) {
        if (!strcmp(fault, "report")) (void)close(STDERR_FILENO);
        failure();
    }
    errno = saved;
}

int mkdir(const char *path, mode_t mode) {
    int (*next)(const char *, mode_t) = dlsym(RTLD_NEXT, "mkdir");
    if (!next) { errno = ENOSYS; return -1; }
    int result = next(path, mode);
    observe("directory", path, mode, result, AT_FDCWD);
    return result;
}

int mkdirat(int directory, const char *path, mode_t mode) {
    int (*next)(int, const char *, mode_t) = dlsym(RTLD_NEXT, "mkdirat");
    if (!next) { errno = ENOSYS; return -1; }
    int result = next(directory, path, mode);
    observe("directory", path, mode, result, directory);
    return result;
}

int mkfifo(const char *path, mode_t mode) {
    int (*next)(const char *, mode_t) = dlsym(RTLD_NEXT, "mkfifo");
    if (!next) { errno = ENOSYS; return -1; }
    int result = next(path, mode);
    observe("fifo", path, mode, result, AT_FDCWD);
    return result;
}
