/* Independent native text observer and ACL-level flag fixture setter.
 * Production uses fstatx/filesec and binary serialisation, not these helpers.
 */
#include <sys/acl.h>
#include <stdio.h>
#include <string.h>
#include <errno.h>

int main(int argc, char **argv) {
    if (argc != 3) return 2;
    acl_t acl = acl_get_file(argv[2], ACL_TYPE_EXTENDED);
    if (!acl) {
        if (errno == ENOENT && strcmp(argv[1], "show") == 0) {
            puts("no ACL");
            return 0;
        }
        perror("acl_get_file");
        return 1;
    }
    int result = 0;
    if (strcmp(argv[1], "no-inherit") == 0) {
        acl_flagset_t flags;
        if (acl_get_flagset_np(acl, &flags) != 0 ||
            acl_add_flag_np(flags, ACL_FLAG_NO_INHERIT) != 0 ||
            acl_set_file(argv[2], ACL_TYPE_EXTENDED, acl) != 0) {
            perror("set no-inherit");
            result = 1;
        }
    } else if (strcmp(argv[1], "show") == 0) {
        char *text = acl_to_text(acl, NULL);
        if (!text) { perror("acl_to_text"); result = 1; }
        else { fputs(text, stdout); acl_free(text); }
    } else result = 2;
    acl_free(acl);
    return result;
}
