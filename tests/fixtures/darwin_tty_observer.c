/* Test-only dyld observer/faults for the actual terminal open and acquired fd.
 * Exact-path selection excludes state/config/log files. Calls from this image
 * reach the originals under dyld interposition, as in darwin_acl_fault.c.
 * No production environment switch or terminal-mode change is involved.
 */
#include <errno.h>
#include <fcntl.h>
#include <libproc.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/proc_info.h>
#include <sys/stat.h>
#include <unistd.h>

#ifdef OBSERVER_CONTROL
/* Negative control: omit O_NOCTTY deliberately on a disposable PTY. No claim
 * that this must acquire a controlling terminal on the running Darwin kernel. */
int main(int argc, char **argv) {
    if (argc != 2) return 1;
    int fd = open(argv[1], O_WRONLY | O_CLOEXEC);
    if (fd < 0) return 2;
    if (write(fd, "control", 7) != 7) return 3;
    return close(fd) != 0;
}
#else
static int target_fd = -1;
static size_t written;

static int fault(const char *name) {
    const char *value = getenv("CCTAB_TEST_TTY_FAULT");
    return value && strcmp(value, name) == 0;
}

static struct proc_bsdinfo state(void) {
    struct proc_bsdinfo info = {0};
    if (proc_pidinfo(getpid(), PROC_PIDTBSDINFO, 0, &info, sizeof(info)) != (int)sizeof(info))
        _exit(90);
    return info;
}

static unsigned controlling(struct proc_bsdinfo info) {
    return info.pbi_flags & (PROC_FLAG_CTTY | PROC_FLAG_CONTROLT);
}

static void record(const char *event, long value) {
    int saved = errno;
    const char *log = getenv("CCTAB_TEST_TTY_LOG");
    if (!log) _exit(91);
    char line[256];
    int n = snprintf(line, sizeof(line), "{\"event\":\"%s\",\"value\":%ld}\n", event, value);
    int out = open(log, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0600);
    if (n < 0 || n >= (int)sizeof(line) || out < 0 || write(out, line, n) != n)
        _exit(92);
    close(out);
    errno = saved;
}

__attribute__((constructor)) static void initial_state(void) {
    record("initial_ctty", controlling(state()));
    record("session_leader", getsid(0) == getpid());
    record("stdio_tty", isatty(0) || isatty(1) || isatty(2));
}

__attribute__((destructor)) static void final_state(void) {
    record("final_ctty", controlling(state()));
    record("unclosed", target_fd >= 0);
}

static int observe_open(const char *path, int flags, ...) {
    mode_t mode = 0;
    if (flags & O_CREAT) {
        va_list args;
        va_start(args, flags);
        mode = (mode_t)va_arg(args, int);
        va_end(args);
    }
    const char *target = getenv("CCTAB_TEST_TTY_PATH");
    if (!target || strcmp(path, target) != 0) return open(path, flags, mode);
    record("before_ctty", controlling(state()));
    record("open_flags", flags);
    record("noctty", (flags & O_NOCTTY) != 0);
    int fd;
    if (fault("open")) { errno = EACCES; fd = -1; }
    else fd = open(path, flags, mode);
    int saved = errno;
    record("after_ctty", controlling(state()));
    record("open_result", fd >= 0);
    if (fd >= 0) {
        int descriptor_flags = fcntl(fd, F_GETFD);
        if (descriptor_flags < 0) _exit(95);
        record("cloexec", (descriptor_flags & FD_CLOEXEC) != 0);
        if (fault("regular") || fault("null")) {
            const char *replacement = fault("null") ? "/dev/null" : getenv("CCTAB_TEST_TTY_FILE");
            if (!replacement) _exit(93);
            close(fd);
            fd = open(replacement, O_WRONLY | O_CLOEXEC);
            if (fd < 0) _exit(94);
        }
        target_fd = fd;
    }
    errno = saved;
    return fd;
}

static int observe_fstat(int fd, struct stat *st) {
    if (fd != target_fd) return fstat(fd, st);
    record("metadata", 1);
    if (fault("metadata")) { errno = EIO; return -1; }
    int result = fstat(fd, st);
    if (result == 0) record("character", S_ISCHR(st->st_mode));
    return result;
}

static int observe_isatty(int fd) {
    if (fd != target_fd) return isatty(fd);
    record("terminal_check", 1);
    if (fault("terminal")) { errno = EIO; return 0; }
    int result = isatty(fd);
    record("terminal", result != 0);
    return result;
}

static ssize_t observe_write(int fd, const void *bytes, size_t size) {
    if (fd != target_fd) return write(fd, bytes, size);
    record("write_attempt", (long)size);
    if (fault("write") || (fault("partial") && written >= 3)) {
        errno = EIO;
        return -1;
    }
    if (fault("partial") && size > 3 - written) size = 3 - written;
    ssize_t n = write(fd, bytes, size);
    if (n > 0) written += (size_t)n;
    record("write_result", n);
    return n;
}

static int observe_close(int fd) {
    if (fd != target_fd) return close(fd);
    target_fd = -1;
    int result = close(fd);
    record("closed", result == 0);
    return result;
}

#define INTERPOSE(replacement, original) \
    __attribute__((used)) static const struct { const void *new_fn; const void *old_fn; } \
    interpose_##original __attribute__((section("__DATA,__interpose"))) = \
        { (const void *)replacement, (const void *)original }

INTERPOSE(observe_open, open);
INTERPOSE(observe_fstat, fstat);
INTERPOSE(observe_isatty, isatty);
INTERPOSE(observe_write, write);
INTERPOSE(observe_close, close);
/* Rust's libc selects the non-cancelling close symbol on Intel Darwin. */
#ifdef __x86_64__
extern int close_nocancel(int) __asm("_close$NOCANCEL");
INTERPOSE(observe_close, close_nocancel);
#endif
#endif
