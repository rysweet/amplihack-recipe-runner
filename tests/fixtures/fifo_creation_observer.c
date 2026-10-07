// Linux-only observational delegates: never change permissions or syscall results.
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

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
    char line[4096];
    int bytes = snprintf(line, sizeof(line), "%s\t%u\t%d\t%u\t%u\t%lu\t%lu\t%s\n",
                         kind, (unsigned)requested, rc,
                         rc == 0 ? (unsigned)info.st_mode : 0,
                         rc == 0 ? (unsigned)info.st_uid : 0,
                         rc == 0 ? (unsigned long)info.st_dev : 0,
                         rc == 0 ? (unsigned long)info.st_ino : 0, path);
    if (bytes > 0 && (size_t)bytes < sizeof(line)) {
        int fd = open(log, O_WRONLY | O_APPEND | O_CREAT | O_CLOEXEC, 0600);
        if (fd >= 0) {
            (void)write(fd, line, (size_t)bytes);
            (void)close(fd);
        }
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
