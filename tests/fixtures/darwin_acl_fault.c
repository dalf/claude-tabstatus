/* Native test-only dyld interposition. No production fault switches.
 * Compile against the runner's SDK to check the opaque API declarations too.
 * fstatx/filesec and ACL entry iteration observe staging independently of the
 * production security snapshot/serialisation helpers.
 */
#include <sys/acl.h>
#include <sys/stat.h>
#include <fcntl.h>
#include <unistd.h>
#include <errno.h>
#include <stdlib.h>
#include <string.h>
#include <copyfile.h>

_Static_assert(ACL_TYPE_EXTENDED == 0x100, "ACL type");
_Static_assert(ACL_FLAG_NO_INHERIT == (1 << 17), "no-inherit flag");
_Static_assert(FILESEC_MODE == 4 && FILESEC_ACL == 5, "filesec properties");
_Static_assert(_PC_EXTENDED_SECURITY_NP == 13, "pathconf selector");

static int fault(const char *name) {
    const char *value = getenv("CCTAB_TEST_ACL_FAULT");
    return value && strcmp(value, name) == 0;
}

static void private_birth(int fd) {
    struct stat st;
    filesec_t fs = filesec_init();
    acl_t acl = NULL;
    acl_entry_t entry = NULL;
    if (!fs || fstatx_np(fd, &st, fs) != 0 || st.st_size != 0 ||
        (st.st_mode & 0777 & ~0600) != 0) {
        _exit(91);
    }
    int got = filesec_get_property(fs, FILESEC_ACL, &acl);
    if ((got != 0 && errno != ENOENT) ||
        /* Darwin returns -1/EINVAL at the end of its ACL iterator. */
        (acl && acl_get_entry(acl, ACL_FIRST_ENTRY, &entry) == 0)) _exit(91);
    if (acl) acl_free(acl);
    filesec_free(fs);
    const char *log = getenv("CCTAB_TEST_ACL_LOG");
    int out = open(log, O_WRONLY | O_CREAT | O_APPEND, 0600);
    if (out < 0 || write(out, "private empty staging\n", 22) != 22) _exit(92);
    close(out);
}

static int fail_get(int fd, struct stat *st, filesec_t fs) {
    if (fault("get") || fault("get_unsupported") || fault("get_enoent")) {
        errno = fault("get") ? EACCES : fault("get_enoent") ? ENOENT : ENOTSUP;
        return -1;
    }
    return fstatx_np(fd, st, fs);
}
static long fail_pathconf(int fd, int name) {
    if (name == _PC_EXTENDED_SECURITY_NP && fault("unsupported")) return 0;
    return fpathconf(fd, name);
}
static int fail_set(int fd, acl_t acl, acl_type_t type) {
    if (fault("set") || fault("lost_acl")) {
        private_birth(fd);
        if (fault("lost_acl")) return 0; /* success without preserving anything */
        errno = EIO;
        return -1;
    }
    return acl_set_fd_np(fd, acl, type);
}
static int fail_clear(int fd, filesec_t fs) {
    if (fault("clear")) {
        private_birth(fd);
        errno = EIO;
        return -1;
    }
    return fchmodx_np(fd, fs);
}
static int fail_open(const char *path, int flags, filesec_t fs) {
    if (fault("open")) { errno = ENOTSUP; return -1; }
    return openx_np(path, flags, fs);
}
static int fail_chmod(const char *path, mode_t mode) {
    struct stat st;
    if (fault("final_mode") && strstr(path, ".cctab-tmp.") &&
        stat(path, &st) == 0 && st.st_size > 0) {
        errno = EIO;
        return -1;
    }
    return chmod(path, mode);
}
static int fail_copy(int from, int to, copyfile_state_t state, copyfile_flags_t flags) {
    if (fault("copy")) {
        if (write(to, "partial", 7) != 7) _exit(93);
        errno = EIO;
        return -1;
    }
    return fcopyfile(from, to, state, flags);
}

#define INTERPOSE(replacement, original) \
    __attribute__((used)) static const struct { const void *new_fn; const void *old_fn; } \
    interpose_##original __attribute__((section("__DATA,__interpose"))) = \
        { (const void *)replacement, (const void *)original }

INTERPOSE(fail_get, fstatx_np);
INTERPOSE(fail_pathconf, fpathconf);
INTERPOSE(fail_set, acl_set_fd_np);
INTERPOSE(fail_clear, fchmodx_np);
INTERPOSE(fail_open, openx_np);
INTERPOSE(fail_chmod, chmod);
INTERPOSE(fail_copy, fcopyfile);
