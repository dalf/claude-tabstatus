/* Independent native kernel observer and interposition positive control.
 * No candidate code, hostname mutation or terminal access is used here.
 * The SDK checks the declaration and the fixed bound used by production.
 */
#include <sys/param.h>
#include <sys/sysctl.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

_Static_assert(MAXHOSTNAMELEN == 256, "review the Darwin hostname bound");

#ifdef RECORD_HOSTNAME
/* An actual PATH-resolved executable, with a positive control before using
 * its missing log as evidence. It has no interpreter or system-command lookup. */
int main(void) {
    const char *log = getenv("CCTAB_TEST_COMMAND_LOG");
    if (!log) return 90;
    int out = open(log, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0600);
    if (out < 0 || write(out, "hostname command\n", 17) != 17) return 91;
    if (close(out) != 0) return 92;
    return puts("command.example") == EOF;
}
#else
int main(int argc, char **argv) {
    unsigned char name[MAXHOSTNAMELEN + 1];
    memset(name, 0xff, sizeof(name));
    if (argc == 2 && strcmp(argv[1], "query") == 0) {
        /* Assigning the SDK declaration checks the locked libc signature. */
        int (*query)(char *, size_t) = gethostname;
        if (query((char *)name, sizeof(name)) != 0) return 2;
    } else if (argc == 1) {
        /* KERN_HOSTNAME is independent of the candidate's gethostname call. */
        int mib[] = {CTL_KERN, KERN_HOSTNAME};
        size_t size = sizeof(name);
        if (sysctl(mib, 2, name, &size, NULL, 0) != 0 || size > sizeof(name)) return 3;
    } else return 4;
    unsigned char *end = memchr(name, 0, sizeof(name));
    if (!end || end == name) return 5;
    size_t count = (size_t)(end - name);
    return fwrite(name, 1, count, stdout) != count;
}
#endif
