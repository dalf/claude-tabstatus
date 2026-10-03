/* Native inspection failures, independent of the production Rust helpers. */
#define _DARWIN_C_SOURCE 1
#include <errno.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/acl.h>
#include <sys/mount.h>
#include <sys/stat.h>

_Static_assert(_PC_CASE_SENSITIVE == 11, "pathconf selector matches locked libc");

static int fault(const char *name) {
    const char *value = getenv("CCTAB_TEST_PATH_FAULT");
    return value && strcmp(value, name) == 0;
}

static int probe_fd(int fd) {
    char path[4096];
    return fcntl(fd, F_GETPATH, path) == 0 && strstr(path, ".cctab-name-probe-");
}

static int fail_mkdirx(const char *path, filesec_t sec) {
    if (!strstr(path, ".cctab-name-probe-")) return mkdirx_np(path, sec);
    if (fault("probe-create")) { errno = EACCES; return -1; }
    int result = mkdirx_np(path, sec);
    if (result == 0 && fault("probe-observe")) {
        int fd = open(path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW);
        struct stat st;
        filesec_t got = filesec_init();
        acl_t acl = NULL;
        acl_entry_t entry = NULL;
        if (fd < 0 || !got || fstatx_np(fd, &st, got) != 0 ||
            !S_ISDIR(st.st_mode) || (st.st_mode & 0777) != 0700 || st.st_uid != geteuid()) _exit(91);
        int read_acl = filesec_get_property(got, FILESEC_ACL, &acl);
        /* Darwin's ACL iterator returns -1/EINVAL at end, 0 for an entry. */
        if ((read_acl != 0 && errno != ENOENT) ||
            (acl && acl_get_entry(acl, ACL_FIRST_ENTRY, &entry) == 0)) _exit(91);
        if (acl) acl_free(acl);
        filesec_free(got);
        close(fd);
        fputs("observed private filename probe\n", stderr);
    }
    return result;
}

static int fail_mkdirat(int fd, const char *name, mode_t mode) {
    if (probe_fd(fd) && fault("probe-child")) { errno = EIO; return -1; }
    return mkdirat(fd, name, mode);
}

static int fail_fstatat(int fd, const char *name, struct stat *st, int flags) {
    if (probe_fd(fd) && fault("probe-lookup")) { errno = EIO; return -1; }
    return fstatat(fd, name, st, flags);
}

static int fail_unlinkat(int fd, const char *name, int flags) {
    static int once = 0;
    if (probe_fd(fd) && (fault("probe-cleanup-persistent") ||
                        (fault("probe-cleanup") && !once++))) { errno = EIO; return -1; }
    return unlinkat(fd, name, flags);
}

static int fail_fstatfs(int fd, struct statfs *st) {
    int result = fstatfs(fd, st);
    if (result == 0 && fault("probe-filesystem")) {
        memset(st->f_fstypename, 0, sizeof(st->f_fstypename));
        memcpy(st->f_fstypename, "other", 5);
    }
    return result;
}

static int fail_fstatx(int fd, struct stat *st, filesec_t sec) {
    if (probe_fd(fd) && fault("probe-acl-inspect")) { errno = EIO; return -1; }
    int result = fstatx_np(fd, st, sec);
    if (result == 0 && probe_fd(fd) && fault("probe-acl-iterate")) {
        /* Guarantee an ACL to iterate even if the filesystem represents the
         * private empty birth ACL as absent. The iterator then fails below. */
        acl_t acl = acl_init(0);
        if (!acl || filesec_set_property(sec, FILESEC_ACL, &acl) != 0) _exit(93);
        acl_free(acl);
    }
    return result;
}

static int fail_acl_entry(acl_t acl, int id, acl_entry_t *entry) {
    if (fault("probe-acl-iterate")) { errno = EIO; return -1; }
    return acl_get_entry(acl, id, entry);
}

static long fail_pathconf(const char *path, int name) {
    const char *fault = getenv("CCTAB_TEST_PATH_FAULT");
    if (name == _PC_CASE_SENSITIVE && fault && strcmp(fault, "case") == 0) {
        errno = ENOTSUP;
        return -1;
    }
    return pathconf(path, name);
}

static char *fail_realpath(const char *path, char *resolved) {
    /* Apple's _stdlib.h selects realpath$DARWIN_EXTSN with _DARWIN_C_SOURCE,
     * matching the symbol used by locked libc and Rust's canonicalize. */
    const char *fault = getenv("CCTAB_TEST_PATH_FAULT");
    if (fault && strcmp(fault, "resolve") == 0 && strstr(path, "/data")) {
        errno = EIO;
        return NULL;
    }
    return realpath(path, resolved);
}

#define INTERPOSE(replacement, original) \
    __attribute__((used)) static const struct { const void *new_fn; const void *old_fn; } \
    interpose_##original __attribute__((section("__DATA,__interpose"))) = \
        { (const void *)replacement, (const void *)original }

INTERPOSE(fail_pathconf, pathconf);
INTERPOSE(fail_realpath, realpath);
INTERPOSE(fail_mkdirx, mkdirx_np);
INTERPOSE(fail_mkdirat, mkdirat);
INTERPOSE(fail_fstatat, fstatat);
INTERPOSE(fail_unlinkat, unlinkat);
INTERPOSE(fail_fstatfs, fstatfs);
INTERPOSE(fail_fstatx, fstatx_np);
INTERPOSE(fail_acl_entry, acl_get_entry);
