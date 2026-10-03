/* Test-only dyld observation and isolated gethostname faults. Calls from this
 * image reach the original, as in the ACL and terminal-open interposers.
 * The environment switches belong to this fixture, never to production.
 */
#include <sys/param.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

_Static_assert(MAXHOSTNAMELEN == 256, "review the Darwin hostname bound");

static int observed_hostname(char *name, size_t size) {
    const char *log = getenv("CCTAB_TEST_HOST_LOG");
    const char *mode = getenv("CCTAB_TEST_HOST_FAULT");
    if (!log || !mode || !name || !size) _exit(90);
    int out = open(log, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0600);
    char line[64];
    int n = snprintf(line, sizeof(line), "gethostname %zu\n", size);
    if (out < 0 || n < 0 || n >= (int)sizeof(line) || write(out, line, n) != n)
        _exit(91);
    if (close(out) != 0) _exit(92);

    if (strcmp(mode, "observe") == 0) return gethostname(name, size);
    if (strcmp(mode, "empty") == 0) { name[0] = 0; return 0; }
    if (strcmp(mode, "unterminated") == 0) { memset(name, 'x', size); return 0; }
    if (strcmp(mode, "partial") == 0) {
        if (size < 4) _exit(93);
        memcpy(name, "host", 4); /* no NUL; untouched storage must not mask it */
        return 0;
    }
    if (strcmp(mode, "boundary") == 0) {
        memset(name, 'b', size);
        name[size - 1] = 0;
        return 0;
    }

    const unsigned char *value;
    size_t count;
    static const unsigned char good[] = "native.example";
    static const unsigned char unicode[] = "h\xc3\xb4te.example";
    static const unsigned char invalid[] = {'s', 0xff, 'v', 0xe2, 0x82, 0};
    static const unsigned char numeric[] = "192.168.1.5";
    static const unsigned char hostile[] = "s\"r\\v\n.example";
    if (strcmp(mode, "success") == 0 || strcmp(mode, "failure") == 0) {
        value = good; count = sizeof(good);
    } else if (strcmp(mode, "unicode") == 0) {
        value = unicode; count = sizeof(unicode);
    } else if (strcmp(mode, "invalid_utf8") == 0) {
        value = invalid; count = sizeof(invalid);
    } else if (strcmp(mode, "numeric") == 0) {
        value = numeric; count = sizeof(numeric);
    } else if (strcmp(mode, "hostile") == 0) {
        value = hostile; count = sizeof(hostile);
    } else _exit(94);
    if (count > size) _exit(95);
    memcpy(name, value, count);
    /* A failure with valid-looking output must still be rejected. */
    if (strcmp(mode, "failure") == 0) { errno = EIO; return -1; }
    return 0;
}

__attribute__((used)) static const struct {
    const void *replacement;
    const void *original;
} interpose[] __attribute__((section("__DATA,__interpose"))) = {
    {(const void *)observed_hostname, (const void *)gethostname},
};
