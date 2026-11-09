/* Disposable native process for restart tests; never invokes the real Codex. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <fcntl.h>
#include <signal.h>
#include <unistd.h>

static volatile sig_atomic_t interrupted;

static void interrupt(int number)
{
    (void)number;
    interrupted = 1;
}

int main(int argc, char **argv)
{
    (void)argc;
    (void)argv;
    if (getenv("APPLICATION_UPDATE_FIXTURE_GRACEFUL_EXIT") != NULL)
        signal(SIGINT, interrupt);
    signal(SIGUSR1, interrupt);
    const char *logs = getenv("APPLICATION_UPDATE_FIXTURE_LOGS");
    if (logs != NULL) {
        char *paths = strdup(logs);
        for (char *path = strtok(paths, ":"); path != NULL; path = strtok(NULL, ":"))
            if (open(path, O_RDONLY) < 0)
                return 2;
        free(paths);
    }
    puts("Isolated Codex restart fixture");
    fflush(stdout);
    while (!interrupted)
        pause();
    sleep(1);
    return 0;
}
