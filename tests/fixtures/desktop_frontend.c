/* Private canonical frontend for frontend-request integration, no GUI or SDK.
 * The native parent owns the canonical socket and the exact unreaped CLI child.
 * Python supplies closed cached bytes only after observing that child's birth.
 */
/* Darwin libproc headers require BSD types even with strict C11 enabled. */
#ifdef __APPLE__
#define _DARWIN_C_SOURCE 1
#else
#define _POSIX_C_SOURCE 200809L
#endif
#include <sys/types.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>
#ifdef __APPLE__
#include <libproc.h>
#endif

static pid_t child = -1;
static void stop(int sig) {
    if (child > 0) {
        kill(child, SIGTERM);
        while (waitpid(child, NULL, 0) < 0 && errno == EINTR) {}
    }
    _exit(128 + sig);
}
static void check(int ok) {
    if (!ok) { perror("private frontend fixture"); stop(SIGABRT); }
}
static void write_all(int fd, const char *bytes, size_t length) {
    while (length) {
        ssize_t used = write(fd, bytes, length);
        if (used < 0 && errno == EINTR) continue;
        check(used > 0);
        bytes += used; length -= (size_t)used;
    }
}
static char *line(void) {
    char *bytes = calloc(8193, 1);
    check(bytes != NULL && fgets(bytes, 8193, stdin) != NULL);
    size_t length = strlen(bytes);
    check(length > 0 && length <= 8192 && bytes[length - 1] == '\n');
    return bytes;
}
static void birth(pid_t pid, char *output, size_t length) {
#ifdef __APPLE__
    struct proc_bsdinfo info;
    check(proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, sizeof(info)) == (int)sizeof(info));
    snprintf(output, length, "%llu:%llu", (unsigned long long)info.pbi_start_tvsec,
             (unsigned long long)info.pbi_start_tvusec);
#else
    char path[64], bytes[4096];
    snprintf(path, sizeof(path), "/proc/%d/stat", pid);
    FILE *file = fopen(path, "r");
    check(file != NULL && fgets(bytes, sizeof(bytes), file) != NULL);
    fclose(file);
    char *after = strrchr(bytes, ')'); check(after != NULL);
    char *save = NULL, *field = strtok_r(after + 2, " ", &save);
    for (int number = 3; number < 22; number++) {
        check(field != NULL); field = strtok_r(NULL, " ", &save);
    }
    check(field != NULL); snprintf(output, length, "%s", field);
#endif
}
int main(int argc, char **argv) {
    check(argc == 3);
    signal(SIGPIPE, SIG_IGN); signal(SIGTERM, stop); signal(SIGALRM, stop); alarm(35);
    int listener = socket(AF_UNIX, SOCK_STREAM, 0); check(listener >= 0);
    check(fcntl(listener, F_SETFD, FD_CLOEXEC) == 0);
    struct sockaddr_un address; memset(&address, 0, sizeof(address));
    address.sun_family = AF_UNIX;
#ifdef __APPLE__
    address.sun_len = sizeof(address);
#endif
    check(strlen(argv[1]) < sizeof(address.sun_path));
    strcpy(address.sun_path, argv[1]);
    check(bind(listener, (struct sockaddr *)&address, sizeof(address)) == 0);
    check(chmod(argv[1], 0600) == 0 && listen(listener, 8) == 0);
    int input[2], output[2]; check(pipe(input) == 0 && pipe(output) == 0);
    child = fork(); check(child >= 0);
    if (child == 0) {
        close(input[1]); close(output[0]);
        check(dup2(input[0], STDIN_FILENO) >= 0 && dup2(output[1], STDOUT_FILENO) >= 0);
        close(input[0]); close(output[1]);
        /* A marked native parent stays visible even if its relay is clean. */
        unsetenv("HGS_RUN_ID");
        execl(argv[2], argv[2], "desktop", "frontend-request", "--json", (char *)NULL);
        _exit(127);
    }
    close(input[0]); close(output[1]);
    char identity[128]; birth(child, identity, sizeof(identity));
    printf("{\"pid\":%d,\"birth\":\"%s\"}\n", child, identity); fflush(stdout);
    char *envelope = line(), *reply = line(), *query = line();
    write_all(input[1], envelope, strlen(envelope)); close(input[1]); free(envelope);
    size_t total = 0; int proofs = 0;
    for (;;) {
        struct pollfd ready[2] = {{listener, POLLIN, 0}, {output[0], POLLIN, 0}};
        int result = poll(ready, 2, 30000);
        if (result < 0 && errno == EINTR) continue;
        check(result > 0);
        if (ready[0].revents & POLLIN) {
            int client = accept(listener, NULL, NULL); check(client >= 0);
            struct pollfd incoming = {client, POLLIN, 0};
            if (poll(&incoming, 1, 1000) == 1) {
                char request[65]; ssize_t used = read(client, request, sizeof(request));
                if (used == (ssize_t)strlen(query) && memcmp(request, query, (size_t)used) == 0) {
                    write_all(client, reply, strlen(reply)); proofs++;
                }
            }
            close(client);
        }
        if (ready[1].revents & (POLLIN | POLLHUP)) {
            char bytes[4096]; ssize_t used = read(output[0], bytes, sizeof(bytes));
            if (used < 0 && errno == EINTR) continue;
            check(used >= 0);
            if (used == 0) break;
            total += (size_t)used; check(total <= 65536);
            write_all(STDOUT_FILENO, bytes, (size_t)used);
        }
    }
    close(output[0]); close(listener); check(unlink(argv[1]) == 0);
    int status; check(waitpid(child, &status, 0) == child); child = -1;
    fprintf(stderr, "private frontend proofs=%d original_reaped=1\n", proofs);
    free(reply); free(query);
    check(WIFEXITED(status)); return WEXITSTATUS(status);
}
