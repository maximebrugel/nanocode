#ifndef _FCNTL_H
#define _FCNTL_H
#define O_RDONLY 0x0
#define O_WRONLY 0x1
#define O_RDWR 0x2
#define O_CREAT 0x40
#define O_EXCL 0x80
#define O_TRUNC 0x200
#define O_APPEND 0x400
int open(const char *path, int flags, ...);
#endif
