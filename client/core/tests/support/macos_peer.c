/* Private qualification peer. Never installed or used by production. */
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>
#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>

static int serve(int connection) {
    char byte;
    while (read(connection, &byte, 1) == 1) {
        if (byte == 'q') return 0;
        if (byte == 'e') {
            const char *target = getenv("ZERUS_MAC_PEER_EXECUTABLE");
            if (!target) return 5;
            char number[32];
            snprintf(number, sizeof(number), "%d", connection);
            if (fcntl(connection, F_SETFD, 0) != 0) return 6;
            execl(target, target, "--after-exec", number, (char *)0);
            return 7;
        }
        if (write(connection, &byte, 1) != 1) return 8;
    }
    return 0;
}

int main(int argc, char **argv) {
    if (argc == 3 && strcmp(argv[1], "--after-exec") == 0) {
        int connection = atoi(argv[2]);
        if (write(connection, "E", 1) != 1) return 9;
        return serve(connection);
    }
    if (argc != 2 || strlen(argv[1]) >= sizeof(((struct sockaddr_un *)0)->sun_path)) return 2;
    int listener = socket(AF_UNIX, SOCK_STREAM, 0);
    struct sockaddr_un address = {0};
#ifdef __APPLE__
    address.sun_len = sizeof(address);
#endif
    address.sun_family = AF_UNIX;
    strcpy(address.sun_path, argv[1]);
    if (listener < 0 || bind(listener, (struct sockaddr *)&address, sizeof(address)) != 0 || listen(listener, 1) != 0) return 3;
    int connection = accept(listener, 0, 0);
    close(listener);
    if (connection < 0) return 4;
    int result = serve(connection);
    close(connection);
    return result;
}
