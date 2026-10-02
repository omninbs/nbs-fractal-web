// Self-extracting zstd launcher.
//
// `sfx pack <exe> <out> [level]` prepends this launcher's own bytes to a zstd
// frame of <exe> and appends a footer, producing a single runnable ELF.
// Running a packed file decompresses the payload into an anonymous memfd and
// execs it in place, so no temporary file ever touches disk.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>
#include <zstd.h>

extern char **environ;

static const char MAGIC[8] = {'N', 'B', 'F', 'S', 'F', 'X', '0', '1'};
#define FOOTER_SIZE 16

static void die(const char *msg) {
    fprintf(stderr, "sfx: %s\n", msg);
    exit(1);
}

static void write_all(int fd, const void *buf, size_t len) {
    const unsigned char *p = buf;
    while (len > 0) {
        ssize_t n = write(fd, p, len);
        if (n <= 0)
            die("write failed");
        p += n;
        len -= (size_t)n;
    }
}

static unsigned char *read_file(const char *path, size_t *len_out) {
    int fd = open(path, O_RDONLY);
    if (fd < 0)
        die("open failed");
    struct stat st;
    if (fstat(fd, &st) != 0)
        die("fstat failed");
    size_t len = (size_t)st.st_size;
    unsigned char *buf = malloc(len ? len : 1);
    if (!buf)
        die("out of memory");
    size_t off = 0;
    while (off < len) {
        ssize_t n = read(fd, buf + off, len - off);
        if (n <= 0)
            die("read failed");
        off += (size_t)n;
    }
    close(fd);
    *len_out = len;
    return buf;
}

static int pack(const char *in_path, const char *out_path, int level) {
    size_t stub_len, src_len;
    unsigned char *stub = read_file("/proc/self/exe", &stub_len);
    unsigned char *src = read_file(in_path, &src_len);

    size_t cap = ZSTD_compressBound(src_len);
    unsigned char *cmp = malloc(cap);
    if (!cmp)
        die("out of memory");
    size_t cmp_len = ZSTD_compress(cmp, cap, src, src_len, level);
    if (ZSTD_isError(cmp_len))
        die(ZSTD_getErrorName(cmp_len));

    int fd = open(out_path, O_WRONLY | O_CREAT | O_TRUNC, 0755);
    if (fd < 0)
        die("cannot create output");
    write_all(fd, stub, stub_len);
    write_all(fd, cmp, cmp_len);
    unsigned char footer[FOOTER_SIZE];
    uint64_t n = (uint64_t)cmp_len;
    memcpy(footer, &n, sizeof(n));
    memcpy(footer + sizeof(n), MAGIC, sizeof(MAGIC));
    write_all(fd, footer, FOOTER_SIZE);
    if (fchmod(fd, 0755) != 0)
        die("chmod failed");
    close(fd);

    fprintf(stderr, "sfx: %zu -> %zu bytes payload (+%zu launcher)\n",
            src_len, cmp_len, stub_len);
    return 0;
}

static int run(char *const argv[]) {
    size_t self_len;
    unsigned char *self = read_file("/proc/self/exe", &self_len);
    if (self_len < FOOTER_SIZE)
        die("truncated file");
    unsigned char *footer = self + self_len - FOOTER_SIZE;
    if (memcmp(footer + 8, MAGIC, 8) != 0) {
        fprintf(stderr, "sfx: this launcher carries no payload\n");
        return 1;
    }
    uint64_t cmp_len;
    memcpy(&cmp_len, footer, sizeof(cmp_len));
    if ((uint64_t)self_len < FOOTER_SIZE + cmp_len)
        die("corrupt payload");
    unsigned char *cmp = self + self_len - FOOTER_SIZE - cmp_len;

    unsigned long long dec_len = ZSTD_getFrameContentSize(cmp, cmp_len);
    if (dec_len == ZSTD_CONTENTSIZE_ERROR)
        die("not a zstd frame");
    if (dec_len == ZSTD_CONTENTSIZE_UNKNOWN)
        die("payload has no declared size");
    unsigned char *dec = malloc((size_t)dec_len);
    if (!dec)
        die("out of memory");
    size_t got = ZSTD_decompress(dec, (size_t)dec_len, cmp, cmp_len);
    if (ZSTD_isError(got))
        die(ZSTD_getErrorName(got));

    int fd = (int)syscall(SYS_memfd_create, "nbs-fractal-web", 0u);
    if (fd < 0)
        die("memfd_create failed");
    write_all(fd, dec, got);
    if (fchmod(fd, 0755) != 0)
        die("chmod failed");

    syscall(SYS_execveat, fd, "", argv, environ, AT_EMPTY_PATH);
    die("execveat failed");
}

int main(int argc, char **argv) {
    if (argc >= 4 && strcmp(argv[1], "pack") == 0) {
        int level = (argc >= 5) ? atoi(argv[4]) : 19;
        return pack(argv[2], argv[3], level);
    }
    return run(argv);
}
