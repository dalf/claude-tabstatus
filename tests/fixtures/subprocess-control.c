/* Linux-only tracer controls. Use raw syscalls so libc cannot substitute clone
 * for fork or execve for execveat. Failed attempts violate the policy too. */
#define _GNU_SOURCE
#include <fcntl.h>
#include <signal.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

int main(int argc, char **argv) {
    if (argc != 2) return 2;
    char *args[] = {"/bin/true", NULL};
    char *env[] = {NULL};
    long pid;
    if (!strcmp(argv[1], "clean")) return 0;
    if (!strcmp(argv[1], "execve")) {
        syscall(SYS_execve, args[0], args, env);
    } else if (!strcmp(argv[1], "execveat")) {
        syscall(SYS_execveat, AT_FDCWD, args[0], args, env, 0);
    } else if (!strcmp(argv[1], "failed-execve")) {
        syscall(SYS_execve, "/no-such-cctab-program", args, env);
    } else if (!strcmp(argv[1], "clone3")) {
        /* Invalid arguments, also observable when the kernel returns ENOSYS. */
        syscall(SYS_clone3, NULL, 0);
    } else if (!strcmp(argv[1], "fork")) {
        pid = syscall(SYS_fork);
        if (pid == 0) _exit(0);
        if (pid > 0) waitpid(pid, NULL, 0);
    } else if (!strcmp(argv[1], "vfork")) {
        /* vfork needs libc's special return sequence: a generic syscall()
         * wrapper's return address can be overwritten on the shared stack. */
        pid = vfork();
        if (pid == 0) _exit(0);
        if (pid > 0) waitpid(pid, NULL, 0);
    } else if (!strcmp(argv[1], "clone")) {
        pid = syscall(SYS_clone, SIGCHLD, NULL, NULL, NULL, 0);
        if (pid == 0) _exit(0);
        if (pid > 0) waitpid(pid, NULL, 0);
    } else {
        return 2;
    }
    return 0;
}
