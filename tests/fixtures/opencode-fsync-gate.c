/* Test-only receipt fsync gate. Only the original private sender can enter. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdarg.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static atomic_int entered;

static void gate(int fd) {
    const char *owner = getenv("ZERUS_FSYNC_OWNER_PID");
    const char *prefix = getenv("ZERUS_FSYNC_PREFIX");
    if (!owner || !prefix || getpid() != (pid_t)strtol(owner, NULL, 10)) return;
    char path[PATH_MAX];
#ifdef __APPLE__
    if (fcntl(fd, F_GETPATH, path) == -1) return;
#else
    char link[64];
    snprintf(link, sizeof(link), "/proc/self/fd/%d", fd);
    ssize_t size = readlink(link, path, sizeof(path) - 1);
    if (size < 0) return;
    path[size] = '\0';
#endif
    if (strncmp(path, prefix, strlen(prefix)) || atomic_exchange(&entered, 1)) return;
    const char *witness = getenv("ZERUS_FSYNC_WITNESS_FD");
    const char *release = getenv("ZERUS_FSYNC_RELEASE_FD");
    if (!witness || !release) _exit(121);
    char message[32];
    int length = snprintf(message, sizeof(message), "%ld\n", (long)getpid());
    ssize_t wrote;
    do { wrote = write(atoi(witness), message, (size_t)length); } while (wrote < 0 && errno == EINTR);
    if (wrote != length) _exit(122);
    char answer;
    ssize_t got;
    do { got = read(atoi(release), &answer, 1); } while (got < 0 && errno == EINTR);
    if (got != 1 || answer != 'r') _exit(123);
}

#ifdef __APPLE__
static int gated_fsync(int fd) {
    gate(fd);
    return fsync(fd);
}
/* Rust's File::sync_all uses F_FULLFSYNC on Darwin. Forward each known
 * argument shape explicitly rather than reading a missing variadic argument. */
static int gated_fcntl(int fd, int command, ...) {
    if (command == F_FULLFSYNC) {
        gate(fd);
        return fcntl(fd, command);
    }
    switch (command) {
        case F_GETFD: case F_GETFL: case F_GETOWN:
            return fcntl(fd, command);
    }
    va_list args;
    va_start(args, command);
    int result;
    switch (command) {
        case F_DUPFD: case F_DUPFD_CLOEXEC: case F_SETFD: case F_SETFL:
        case F_SETOWN: case F_NOCACHE: case F_RDAHEAD: case F_SETNOSIGPIPE:
            result = fcntl(fd, command, va_arg(args, int));
            break;
        case F_GETLK: case F_SETLK: case F_SETLKW: case F_GETPATH: case F_PREALLOCATE:
            result = fcntl(fd, command, va_arg(args, void *));
            break;
        default:
            /* An unreviewed ABI must fail the fixture, never guess a type. */
            _exit(124);
    }
    va_end(args);
    return result;
}
__attribute__((used)) static struct { const void *replacement; const void *original; }
interpose[] __attribute__((section("__DATA,__interpose"))) = {
    {(const void *)&gated_fsync, (const void *)&fsync},
    {(const void *)&gated_fcntl, (const void *)&fcntl}
};
#else
int fsync(int fd) {
    gate(fd);
    int (*original)(int) = (int (*)(int))dlsym(RTLD_NEXT, "fsync");
    return original(fd);
}

int fdatasync(int fd) {
    gate(fd);
    int (*original)(int) = (int (*)(int))dlsym(RTLD_NEXT, "fdatasync");
    return original(fd);
}
#endif
