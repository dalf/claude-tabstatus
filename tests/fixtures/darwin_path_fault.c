/* Native inspection failures, independent of the production Rust helpers. */
#define _DARWIN_C_SOURCE 1
#include <errno.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

_Static_assert(_PC_CASE_SENSITIVE == 11, "pathconf selector matches locked libc");

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
