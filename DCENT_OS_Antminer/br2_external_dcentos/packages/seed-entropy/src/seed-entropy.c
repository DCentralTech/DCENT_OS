/* SPDX-License-Identifier: GPL-3.0-or-later */
/*
 * seed-entropy - consume one saved seed, credit it, and persist its successor.
 *
 * Security contract
 * -----------------
 * A seed is renamed to a private consumed name and that directory mutation is
 * fsync()ed before RNDADDENTROPY is attempted.  Once this happens, the old seed
 * is never credited by this program again.  A crash between consumption and a
 * durable replacement intentionally sacrifices availability rather than risk
 * replaying entropy.
 *
 * After RNDADDENTROPY succeeds, the consumed seed is renamed to a credited
 * name and the directory is synced.  That marker proves a later invocation
 * may generate a successor without crediting the old bytes again.  A crash
 * before the marker is visible remains conservatively ambiguous.
 *
 * Before the accounting ioctl, saved bytes are also written to the verified
 * /dev/urandom device for ordinary no-credit kernel mixing.  This is separate
 * from RNDADDENTROPY because Linux 4.4 input-pool accounting and nonblocking
 * pool initialization are not the same state.
 *
 * After a successful ioctl, and only after a separate nonblocking CRNG
 * readiness proof, exactly 512 replacement bytes are read from /dev/urandom.
 * The replacement is written as a 0600 file in the same directory, fsync()ed,
 * and atomically linked at the public seed name.  Directory fsyncs make the
 * state transitions durable.  The temporary hard link remains until removal
 * of the credited seed is durable, so restart can distinguish a replacement
 * created by this program from an unrelated file.
 *
 * Every newly generated public seed also receives a versioned marker
 * containing its boot epoch and SHA-256 digest before its recovery proofs are
 * removed.  The binding prevents a different payload from borrowing the
 * marker's credit provenance.  A same-boot retry therefore cannot credit
 * output from the current kernel CRNG back into that CRNG as independent
 * entropy.  The marker
 * is written and synced under a temporary name before an atomic rename makes
 * it authoritative.
 *
 * The seed path must be absolute and symlink-free.  Every directory component
 * and seed-state file must be root-owned and inaccessible to group/other
 * writers; seed files must be regular, 0600, singly linked, and exactly 512
 * bytes (except for the intentionally double-linked install state).  Unsafe or
 * ambiguous state and obvious constant-byte corruption fail closed.  This is
 * not a statistical entropy estimator.  Concurrent instances are serialized
 * with a nonblocking flock on the seed directory.
 *
 * Markerless seeds are treated as untrusted migration input because earlier
 * releases could write them before proving CRNG readiness.  Their bytes are
 * mixed without credit only after the public name is durably removed; a fresh
 * seed is then created from an authoritatively ready current CRNG.
 *
 * These guarantees assume fsync-correct persistent storage and a kernel that
 * implements RNDADDENTROPY correctly.  They do not claim first-boot entropy,
 * survive malicious root, or recover availability after every possible crash.
 *
 * Initialization mode is deliberately separate from boot-time crediting:
 *
 *   seed-entropy --initialize-if-missing /absolute/seed-file
 *
 * It is intended for orderly shutdown.  Before generating a first seed, the
 * helper proves that getrandom(GRND_NONBLOCK) succeeds.  Kernels without that
 * authoritative readiness interface fail closed: an input-pool entropy count
 * is not equivalent to nonblocking-CRNG initialization.  It never credits
 * entropy and never replaces a valid existing seed.  It only reads
 * /dev/urandom when authoritative lifecycle names are absent or a durable
 * credited name proves that successor generation may resume, and only after
 * the readiness proof succeeds.  An uninstalled successor may be discarded
 * and regenerated; an interrupted same-inode public+witness install is first
 * protected by a transactional boot-epoch marker and then completed.  Every
 * other partial state fails closed.
 *
 * Usage: seed-entropy [absolute-seed-file]
 *        seed-entropy --initialize-if-missing absolute-seed-file
 *        seed-entropy --initialize-if-missing-at directory-fd seed-basename
 * Default credit seed: /data/keys/random-seed
 */

#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <linux/random.h>
#include <stdint.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/ioctl.h>
#include <sys/sysmacros.h>
#include <sys/syscall.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

#ifndef O_CLOEXEC
#define O_CLOEXEC 0
#endif
#ifndef O_DIRECTORY
#define O_DIRECTORY 0
#endif
#ifndef O_NOFOLLOW
#define O_NOFOLLOW 0
#endif
#ifndef O_NOATIME
#define O_NOATIME 0
#endif
#ifndef AT_SYMLINK_NOFOLLOW
#define AT_SYMLINK_NOFOLLOW 0x100
#endif
#ifndef PATH_MAX
#define PATH_MAX 4096
#endif
#ifndef NAME_MAX
#define NAME_MAX 255
#endif
#ifndef GRND_NONBLOCK
#define GRND_NONBLOCK 0x0001
#endif

#define SEED_SIZE 512
#define CREDIT_BITS 256
#define BOOT_ID_LENGTH 36
#define SHA256_DIGEST_SIZE 32
#define SHA256_HEX_SIZE (SHA256_DIGEST_SIZE * 2)
#define BIRTH_MARKER_VERSION "v1"
#define BIRTH_MARKER_VERSION_SIZE 2
#define BIRTH_MARKER_BOOT_OFFSET (BIRTH_MARKER_VERSION_SIZE + 1)
#define BIRTH_MARKER_DIGEST_OFFSET \
    (BIRTH_MARKER_BOOT_OFFSET + BOOT_ID_LENGTH + 1)
#define BIRTH_MARKER_SIZE (BIRTH_MARKER_DIGEST_OFFSET + SHA256_HEX_SIZE)

static const char *const default_seed = "/data/keys/random-seed";

struct fixed_rand_pool_info {
    int entropy_count;
    int buf_size;
    unsigned char buf[SEED_SIZE];
};

struct sha256_context {
    uint32_t state[8];
    uint64_t bit_count;
    unsigned char block[64];
    size_t block_length;
};

static void secure_zero(void *ptr, size_t len);

static uint32_t sha256_rotate_right(uint32_t value, unsigned int count)
{
    return (value >> count) | (value << (32U - count));
}

static uint32_t sha256_load_be32(const unsigned char *input)
{
    return ((uint32_t)input[0] << 24) |
           ((uint32_t)input[1] << 16) |
           ((uint32_t)input[2] << 8) |
           (uint32_t)input[3];
}

static void sha256_store_be32(unsigned char *output, uint32_t value)
{
    output[0] = (unsigned char)(value >> 24);
    output[1] = (unsigned char)(value >> 16);
    output[2] = (unsigned char)(value >> 8);
    output[3] = (unsigned char)value;
}

static void sha256_transform(struct sha256_context *context,
                             const unsigned char block[64])
{
    static const uint32_t constants[64] = {
        0x428a2f98U, 0x71374491U, 0xb5c0fbcfU, 0xe9b5dba5U,
        0x3956c25bU, 0x59f111f1U, 0x923f82a4U, 0xab1c5ed5U,
        0xd807aa98U, 0x12835b01U, 0x243185beU, 0x550c7dc3U,
        0x72be5d74U, 0x80deb1feU, 0x9bdc06a7U, 0xc19bf174U,
        0xe49b69c1U, 0xefbe4786U, 0x0fc19dc6U, 0x240ca1ccU,
        0x2de92c6fU, 0x4a7484aaU, 0x5cb0a9dcU, 0x76f988daU,
        0x983e5152U, 0xa831c66dU, 0xb00327c8U, 0xbf597fc7U,
        0xc6e00bf3U, 0xd5a79147U, 0x06ca6351U, 0x14292967U,
        0x27b70a85U, 0x2e1b2138U, 0x4d2c6dfcU, 0x53380d13U,
        0x650a7354U, 0x766a0abbU, 0x81c2c92eU, 0x92722c85U,
        0xa2bfe8a1U, 0xa81a664bU, 0xc24b8b70U, 0xc76c51a3U,
        0xd192e819U, 0xd6990624U, 0xf40e3585U, 0x106aa070U,
        0x19a4c116U, 0x1e376c08U, 0x2748774cU, 0x34b0bcb5U,
        0x391c0cb3U, 0x4ed8aa4aU, 0x5b9cca4fU, 0x682e6ff3U,
        0x748f82eeU, 0x78a5636fU, 0x84c87814U, 0x8cc70208U,
        0x90befffaU, 0xa4506cebU, 0xbef9a3f7U, 0xc67178f2U
    };
    uint32_t words[64];
    uint32_t a;
    uint32_t b;
    uint32_t c;
    uint32_t d;
    uint32_t e;
    uint32_t f;
    uint32_t g;
    uint32_t h;
    size_t index;

    for (index = 0; index < 16; index++)
        words[index] = sha256_load_be32(block + index * 4);
    for (index = 16; index < 64; index++) {
        uint32_t s0 = sha256_rotate_right(words[index - 15], 7) ^
                      sha256_rotate_right(words[index - 15], 18) ^
                      (words[index - 15] >> 3);
        uint32_t s1 = sha256_rotate_right(words[index - 2], 17) ^
                      sha256_rotate_right(words[index - 2], 19) ^
                      (words[index - 2] >> 10);

        words[index] = words[index - 16] + s0 + words[index - 7] + s1;
    }

    a = context->state[0];
    b = context->state[1];
    c = context->state[2];
    d = context->state[3];
    e = context->state[4];
    f = context->state[5];
    g = context->state[6];
    h = context->state[7];
    for (index = 0; index < 64; index++) {
        uint32_t sum1 = sha256_rotate_right(e, 6) ^
                        sha256_rotate_right(e, 11) ^
                        sha256_rotate_right(e, 25);
        uint32_t choose = (e & f) ^ ((~e) & g);
        uint32_t temporary1 = h + sum1 + choose + constants[index] +
                              words[index];
        uint32_t sum0 = sha256_rotate_right(a, 2) ^
                        sha256_rotate_right(a, 13) ^
                        sha256_rotate_right(a, 22);
        uint32_t majority = (a & b) ^ (a & c) ^ (b & c);
        uint32_t temporary2 = sum0 + majority;

        h = g;
        g = f;
        f = e;
        e = d + temporary1;
        d = c;
        c = b;
        b = a;
        a = temporary1 + temporary2;
    }
    context->state[0] += a;
    context->state[1] += b;
    context->state[2] += c;
    context->state[3] += d;
    context->state[4] += e;
    context->state[5] += f;
    context->state[6] += g;
    context->state[7] += h;
}

static void sha256_init(struct sha256_context *context)
{
    static const uint32_t initial_state[8] = {
        0x6a09e667U, 0xbb67ae85U, 0x3c6ef372U, 0xa54ff53aU,
        0x510e527fU, 0x9b05688cU, 0x1f83d9abU, 0x5be0cd19U
    };

    memcpy(context->state, initial_state, sizeof(initial_state));
    context->bit_count = 0;
    context->block_length = 0;
    memset(context->block, 0, sizeof(context->block));
}

static void sha256_update(struct sha256_context *context,
                          const unsigned char *input, size_t length)
{
    context->bit_count += (uint64_t)length * 8U;
    while (length != 0) {
        size_t available = sizeof(context->block) - context->block_length;
        size_t copied = length < available ? length : available;

        memcpy(context->block + context->block_length, input, copied);
        context->block_length += copied;
        input += copied;
        length -= copied;
        if (context->block_length == sizeof(context->block)) {
            sha256_transform(context, context->block);
            context->block_length = 0;
        }
    }
}

static void sha256_final(struct sha256_context *context,
                         unsigned char digest[SHA256_DIGEST_SIZE])
{
    uint64_t bit_count = context->bit_count;
    size_t index = context->block_length;

    context->block[index++] = 0x80;
    if (index > 56) {
        memset(context->block + index, 0, sizeof(context->block) - index);
        sha256_transform(context, context->block);
        index = 0;
    }
    memset(context->block + index, 0, 56 - index);
    for (index = 0; index < 8; index++)
        context->block[63 - index] = (unsigned char)(bit_count >> (index * 8));
    sha256_transform(context, context->block);
    for (index = 0; index < 8; index++)
        sha256_store_be32(digest + index * 4, context->state[index]);
}

static void sha256_buffer(const unsigned char *input, size_t length,
                          unsigned char digest[SHA256_DIGEST_SIZE])
{
    struct sha256_context context;

    sha256_init(&context);
    sha256_update(&context, input, length);
    sha256_final(&context, digest);
    secure_zero(&context, sizeof(context));
}

static void sha256_hex(const unsigned char digest[SHA256_DIGEST_SIZE],
                       char output[SHA256_HEX_SIZE + 1])
{
    static const char digits[] = "0123456789abcdef";
    size_t index;

    for (index = 0; index < SHA256_DIGEST_SIZE; index++) {
        output[index * 2] = digits[digest[index] >> 4];
        output[index * 2 + 1] = digits[digest[index] & 0x0f];
    }
    output[SHA256_HEX_SIZE] = '\0';
}

static int sha256_self_test(void)
{
    static const char empty_digest[] =
        "e3b0c44298fc1c149afbf4c8996fb924"
        "27ae41e4649b934ca495991b7852b855";
    static const char abc_digest[] =
        "ba7816bf8f01cfea414140de5dae2223"
        "b00361a396177a9cb410ff61f20015ad";
    unsigned char digest[SHA256_DIGEST_SIZE];
    char hexadecimal[SHA256_HEX_SIZE + 1];
    int result = -1;

    memset(digest, 0, sizeof(digest));
    memset(hexadecimal, 0, sizeof(hexadecimal));
    sha256_buffer((const unsigned char *)"", 0, digest);
    sha256_hex(digest, hexadecimal);
    if (strcmp(hexadecimal, empty_digest) != 0)
        goto out;
    sha256_buffer((const unsigned char *)"abc", 3, digest);
    sha256_hex(digest, hexadecimal);
    if (strcmp(hexadecimal, abc_digest) != 0)
        goto out;
    result = 0;

out:
    secure_zero(digest, sizeof(digest));
    secure_zero(hexadecimal, sizeof(hexadecimal));
    return result;
}

static void secure_zero(void *ptr, size_t len)
{
    volatile unsigned char *p = (volatile unsigned char *)ptr;

    while (len-- != 0)
        *p++ = 0;
}

static int fail(const char *format, ...)
{
    va_list args;

    fprintf(stderr, "seed-entropy: ");
    va_start(args, format);
    vfprintf(stderr, format, args);
    va_end(args);
    fputc('\n', stderr);
    return -1;
}

static int fail_errno(const char *operation)
{
    return fail("%s: %s", operation, strerror(errno));
}

static int owner_is_trusted(uid_t uid)
{
#ifdef SEED_ENTROPY_TESTING
    return uid == 0 || uid == geteuid();
#else
    return uid == 0;
#endif
}

static int validate_directory_stat(const struct stat *st, const char *label)
{
    if (!S_ISDIR(st->st_mode))
        return fail("%s is not a directory", label);
    if (!owner_is_trusted(st->st_uid))
        return fail("%s is not owned by a trusted uid", label);
    if ((st->st_mode & 0022) != 0)
        return fail("%s is writable by group or other", label);
    return 0;
}

/*
 * Walk from an already-open root directory.  O_NOFOLLOW on every openat()
 * prevents an intermediate symlink from redirecting the state machine.
 */
static int open_secure_parent(const char *path, char *basename_out,
                              size_t basename_size)
{
    char copy[PATH_MAX];
    char *last_slash;
    char *cursor;
    int dirfd = -1;
    struct stat st;
    size_t path_len;

    if (path == NULL || path[0] != '/')
        return fail("seed path must be absolute");

    path_len = strlen(path);
    if (path_len < 2 || path_len >= sizeof(copy) || path[path_len - 1] == '/')
        return fail("seed path length or trailing slash is unsafe");
    if (strstr(path, "//") != NULL)
        return fail("seed path contains an empty component");

    memcpy(copy, path, path_len + 1);
    last_slash = strrchr(copy, '/');
    if (last_slash == NULL || last_slash[1] == '\0')
        return fail("seed path has no basename");
    if (strcmp(last_slash + 1, ".") == 0 || strcmp(last_slash + 1, "..") == 0)
        return fail("seed basename is unsafe");
    if (strlen(last_slash + 1) >= basename_size)
        return fail("seed basename is too long");
    strcpy(basename_out, last_slash + 1);
    if (last_slash == copy)
        copy[1] = '\0';
    else
        *last_slash = '\0';

    dirfd = open("/", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
    if (dirfd < 0)
        return fail_errno("open root directory");
    if (fstat(dirfd, &st) != 0 || validate_directory_stat(&st, "/") != 0)
        goto error;

    cursor = copy + 1;
    while (*cursor != '\0') {
        char *separator = strchr(cursor, '/');
        int nextfd;

        if (separator != NULL)
            *separator = '\0';
        if (cursor[0] == '\0' || strcmp(cursor, ".") == 0 ||
            strcmp(cursor, "..") == 0) {
            fail("seed path contains an unsafe component");
            goto error;
        }

        nextfd = openat(dirfd, cursor,
                        O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
        if (nextfd < 0) {
            fail_errno("open seed directory component");
            goto error;
        }
        if (fstat(nextfd, &st) != 0) {
            fail_errno("stat seed directory component");
            close(nextfd);
            goto error;
        }
        if (validate_directory_stat(&st, cursor) != 0) {
            close(nextfd);
            goto error;
        }
        close(dirfd);
        dirfd = nextfd;

        if (separator == NULL)
            break;
        cursor = separator + 1;
    }

    if (flock(dirfd, LOCK_EX | LOCK_NB) != 0) {
        fail_errno("lock seed directory");
        goto error;
    }
    return dirfd;

error:
    if (dirfd >= 0)
        close(dirfd);
    return -1;
}

/*
 * Adopt an already-open directory as the authority boundary.  This supports
 * temporary sysupgrade mountpoints below a shared ancestor such as /tmp
 * without weakening path-based admission: the caller must pass the directory
 * descriptor itself, and the selected directory still has to satisfy the
 * ownership, permission, and single-writer rules.
 */
static int open_secure_directory_fd(int supplied_fd)
{
    struct stat supplied_st;
    struct stat reopened_st;
    int dirfd;

    if (supplied_fd < 3)
        return fail("seed directory descriptor must not be a standard stream");
    if (fstat(supplied_fd, &supplied_st) != 0)
        return fail_errno("stat supplied seed directory descriptor");
    if (validate_directory_stat(&supplied_st,
                                "supplied seed directory descriptor") != 0)
        return -1;

    /*
     * dup() would share the caller's open-file description.  flock() locks
     * are owned by that description, so a caller already holding the lock
     * would be mistaken for this process and exclusion would silently fail.
     * Reopen "." through the trusted descriptor to obtain an independent OFD.
     */
    dirfd = openat(supplied_fd, ".",
                   O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
    if (dirfd < 0)
        return fail_errno("reopen seed directory descriptor");
    if (fstat(dirfd, &reopened_st) != 0) {
        fail_errno("stat reopened seed directory descriptor");
        close(dirfd);
        return -1;
    }
    if (supplied_st.st_dev != reopened_st.st_dev ||
        supplied_st.st_ino != reopened_st.st_ino) {
        fail("reopened seed directory descriptor changed identity");
        close(dirfd);
        return -1;
    }
    if (validate_directory_stat(&reopened_st,
                                "reopened seed directory descriptor") != 0) {
        close(dirfd);
        return -1;
    }
    if (flock(dirfd, LOCK_EX | LOCK_NB) != 0) {
        fail_errno("lock seed directory descriptor");
        close(dirfd);
        return -1;
    }
    return dirfd;
}

static int copy_safe_basename(const char *name, char *output,
                              size_t output_size)
{
    size_t length;

    if (name == NULL || name[0] == '\0' || strchr(name, '/') != NULL ||
        strcmp(name, ".") == 0 || strcmp(name, "..") == 0)
        return fail("seed basename is unsafe");
    length = strlen(name);
    if (length >= output_size)
        return fail("seed basename is too long");
    memcpy(output, name, length + 1);
    return 0;
}

/* Return 1 when present, 0 when absent, and -1 on any other error. */
static int stat_entry(int dirfd, const char *name, struct stat *st)
{
    if (fstatat(dirfd, name, st, AT_SYMLINK_NOFOLLOW) == 0)
        return 1;
    if (errno == ENOENT)
        return 0;
    fail_errno("inspect seed state");
    return -1;
}

static int validate_seed_stat(const struct stat *st, const char *label,
                              nlink_t expected_links)
{
    if (!S_ISREG(st->st_mode))
        return fail("%s is not a regular file", label);
    if (!owner_is_trusted(st->st_uid))
        return fail("%s is not owned by a trusted uid", label);
    if ((st->st_mode & 0777) != 0600)
        return fail("%s mode is not 0600", label);
    if (st->st_size != SEED_SIZE)
        return fail("%s size is not exactly %u bytes", label,
                    (unsigned int)SEED_SIZE);
    if (st->st_nlink != expected_links)
        return fail("%s has an unexpected hard-link count", label);
    return 0;
}

static int same_inode(const struct stat *a, const struct stat *b)
{
    return a->st_dev == b->st_dev && a->st_ino == b->st_ino;
}

static int sync_directory(int dirfd, const char *stage)
{
#ifdef SEED_ENTROPY_TESTING
    const char *failed_stage = getenv("SEED_ENTROPY_TEST_FAIL_DIRECTORY_SYNC");

    if (failed_stage != NULL && strcmp(failed_stage, stage) == 0) {
        errno = EIO;
        return fail_errno(stage);
    }
#endif
    if (fsync(dirfd) != 0)
        return fail_errno(stage);
    return 0;
}

static int sync_seed_file(int fd, const char *stage)
{
#ifdef SEED_ENTROPY_TESTING
    const char *fail_sync = getenv("SEED_ENTROPY_TEST_FAIL_FILE_SYNC");

    if (fail_sync != NULL &&
        (strcmp(fail_sync, "1") == 0 || strcmp(fail_sync, stage) == 0)) {
        errno = EIO;
        return -1;
    }
#else
    (void)stage;
#endif
    return fsync(fd);
}

#ifdef SEED_ENTROPY_TESTING
static void test_failpoint(const char *name)
{
    const char *selected = getenv("SEED_ENTROPY_TEST_FAILPOINT");

    if (selected != NULL && strcmp(selected, name) == 0)
        _exit(90);
}

static const char *random_device_path(void)
{
    const char *path = getenv("SEED_ENTROPY_TEST_RANDOM_PATH");

    return path != NULL ? path : "/dev/urandom";
}
#else
static void test_failpoint(const char *name)
{
    (void)name;
}

static const char *random_device_path(void)
{
    return "/dev/urandom";
}
#endif

static int open_random_device(int flags)
{
#ifdef SEED_ENTROPY_TESTING
    const char *log_path = getenv("SEED_ENTROPY_TEST_RANDOM_OPEN_LOG");

    if (log_path != NULL) {
        const char record[2] = { (flags & O_WRONLY) != 0 ? 'W' : 'R', '\n' };
        int logfd = open(log_path,
                         O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0600);

        if (logfd < 0)
            return -1;
        if (write(logfd, record, sizeof(record)) != (ssize_t)sizeof(record)) {
            int saved_errno = errno != 0 ? errno : EIO;
            close(logfd);
            errno = saved_errno;
            return -1;
        }
        if (close(logfd) != 0)
            return -1;
    }
#endif
    return open(random_device_path(), flags | O_NOFOLLOW | O_CLOEXEC);
}

static int validate_random_metadata(const struct stat *st)
{
    if (!S_ISCHR(st->st_mode))
        return fail("/dev/urandom is not a character device");
    if (major(st->st_rdev) != 1 || minor(st->st_rdev) != 9)
        return fail("/dev/urandom has an unexpected device identity");
    return 0;
}

static int validate_random_fd(int fd)
{
    struct stat st;

#ifdef SEED_ENTROPY_TESTING
    const char *state = getenv("SEED_ENTROPY_TEST_RANDOM_DEVICE");

    (void)fd;
    memset(&st, 0, sizeof(st));
    st.st_mode = S_IFCHR | 0600;
    st.st_rdev = makedev(1, 9);
    if (state != NULL && strcmp(state, "wrong-type") == 0)
        st.st_mode = S_IFREG | 0600;
    else if (state != NULL && strcmp(state, "wrong-rdev") == 0)
        st.st_rdev = makedev(1, 8);
#else
    if (fstat(fd, &st) != 0)
        return fail_errno("stat /dev/urandom");
#endif
    return validate_random_metadata(&st);
}

static ssize_t getrandom_nonblocking_probe(unsigned char *probe)
{
#ifdef SEED_ENTROPY_TESTING
    const char *state = getenv("SEED_ENTROPY_TEST_READINESS");

    if (state == NULL || strcmp(state, "ready") == 0) {
        *probe = 0xa5;
        return 1;
    }
    if (strcmp(state, "not-ready") == 0)
        errno = EAGAIN;
    else if (strcmp(state, "unsupported") == 0)
        errno = ENOSYS;
    else if (strcmp(state, "probe-error") == 0)
        errno = EIO;
    else
        errno = EINVAL;
    return -1;
#else
#ifdef SYS_getrandom
    return syscall(SYS_getrandom, probe, 1, GRND_NONBLOCK);
#else
    (void)probe;
    errno = ENOSYS;
    return -1;
#endif
#endif
}

/*
 * Prove that bytes produced by the kernel CRNG are initialized before they
 * are persisted as a seed that a later boot will credit.  A nonblocking
 * getrandom() is the authoritative readiness test.  RNDGETENTCNT cannot be a
 * fallback because it reports input-pool accounting, not the independent
 * nonblocking-pool initialization state used by Linux 4.4 getrandom().
 */
static int random_source_ready(int random_fd)
{
    unsigned char probe = 0;
    ssize_t count;
    int saved_errno;

    (void)random_fd;

    do {
        count = getrandom_nonblocking_probe(&probe);
    } while (count < 0 && errno == EINTR);
    saved_errno = errno;
    secure_zero(&probe, sizeof(probe));

    if (count == 1)
        return 0;
    if (count < 0 && saved_errno == EAGAIN)
        return fail("kernel random pool is not initialized");
    if (count < 0 && saved_errno == ENOSYS)
        return fail("kernel lacks an authoritative nonblocking CRNG readiness probe");
    if (count >= 0)
        return fail("getrandom readiness probe returned %ld bytes",
                    (long)count);
    errno = saved_errno;
    return fail_errno("probe kernel random-pool readiness");
}

static int add_entropy(int random_fd, struct fixed_rand_pool_info *pool)
{
    if (pool->entropy_count != CREDIT_BITS || pool->buf_size != SEED_SIZE) {
        errno = EINVAL;
        return -1;
    }
#ifdef SEED_ENTROPY_TESTING
    const char *fail_ioctl = getenv("SEED_ENTROPY_TEST_IOCTL_FAIL");
    const char *log_path = getenv("SEED_ENTROPY_TEST_IOCTL_LOG");
    const char *metadata_path = getenv("SEED_ENTROPY_TEST_IOCTL_META_LOG");
    int logfd;
    size_t offset = 0;

    (void)random_fd;
    if (fail_ioctl != NULL && strcmp(fail_ioctl, "0") != 0) {
        errno = EPERM;
        return -1;
    }
    if (log_path == NULL) {
        errno = EINVAL;
        return -1;
    }
    logfd = open(log_path, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0600);
    if (logfd < 0)
        return -1;
    while (offset < (size_t)pool->buf_size) {
        ssize_t written = write(logfd, pool->buf + offset,
                                (size_t)pool->buf_size - offset);
        if (written < 0 && errno == EINTR)
            continue;
        if (written <= 0) {
            int saved_errno = written == 0 ? EIO : errno;
            close(logfd);
            errno = saved_errno;
            return -1;
        }
        offset += (size_t)written;
    }
    if (fsync(logfd) != 0) {
        int saved_errno = errno;
        close(logfd);
        errno = saved_errno;
        return -1;
    }
    if (close(logfd) != 0)
        return -1;
    if (metadata_path != NULL) {
        char record[64];
        int length = snprintf(record, sizeof(record), "%d %d\n",
                              pool->entropy_count, pool->buf_size);

        if (length < 0 || (size_t)length >= sizeof(record)) {
            errno = EOVERFLOW;
            return -1;
        }
        logfd = open(metadata_path,
                     O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0600);
        if (logfd < 0)
            return -1;
        if (write(logfd, record, (size_t)length) != length ||
            fsync(logfd) != 0) {
            int saved_errno = errno != 0 ? errno : EIO;

            close(logfd);
            errno = saved_errno;
            return -1;
        }
        if (close(logfd) != 0)
            return -1;
    }
    return 0;
#else
    return ioctl(random_fd, RNDADDENTROPY, pool);
#endif
}

static int read_exact(int fd, unsigned char *buffer, size_t length)
{
    size_t offset = 0;

    while (offset < length) {
        ssize_t count = read(fd, buffer + offset, length - offset);
        if (count < 0 && errno == EINTR)
            continue;
        if (count < 0)
            return -1;
        if (count == 0) {
            errno = EIO;
            return -1;
        }
        offset += (size_t)count;
    }
    return 0;
}

static int write_exact(int fd, const unsigned char *buffer, size_t length)
{
    size_t offset = 0;

    while (offset < length) {
        ssize_t count = write(fd, buffer + offset, length - offset);
        if (count < 0 && errno == EINTR)
            continue;
        if (count < 0)
            return -1;
        if (count == 0) {
            errno = EIO;
            return -1;
        }
        offset += (size_t)count;
    }
    return 0;
}

/*
 * Linux 4.4 RNDADDENTROPY credits the input pool, while getrandom() is gated
 * by the independently initialized nonblocking pool.  A normal write to the
 * verified urandom device performs the kernel's ordinary no-credit mixing;
 * keep that operation distinct from the accounting ioctl and test it
 * independently.
 */
static int mix_seed_without_credit(int random_fd,
                                   const unsigned char seed[SEED_SIZE])
{
#ifdef SEED_ENTROPY_TESTING
    const char *log_path = getenv("SEED_ENTROPY_TEST_MIX_LOG");
    int logfd;

    (void)random_fd;
    if (log_path == NULL) {
        errno = EINVAL;
        return -1;
    }
    logfd = open(log_path, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0600);
    if (logfd < 0)
        return -1;
    if (write_exact(logfd, seed, SEED_SIZE) != 0 || fsync(logfd) != 0) {
        int saved_errno = errno != 0 ? errno : EIO;

        close(logfd);
        errno = saved_errno;
        return -1;
    }
    if (close(logfd) != 0)
        return -1;
    return 0;
#else
    return write_exact(random_fd, seed, SEED_SIZE);
#endif
}

static int boot_id_is_valid(const char *value)
{
    size_t index;

    for (index = 0; index < BOOT_ID_LENGTH; index++) {
        unsigned char byte = (unsigned char)value[index];

        if (index == 8 || index == 13 || index == 18 || index == 23) {
            if (byte != '-')
                return 0;
        } else if (!((byte >= '0' && byte <= '9') ||
                     (byte >= 'a' && byte <= 'f'))) {
            return 0;
        }
    }
    return value[BOOT_ID_LENGTH] == '\0';
}

static int read_current_boot_id(char output[BOOT_ID_LENGTH + 1])
{
#ifdef SEED_ENTROPY_TESTING
    const char *value = getenv("SEED_ENTROPY_TEST_BOOT_ID");

    if (value == NULL)
        value = "11111111-1111-4111-8111-111111111111";
    if (strlen(value) != BOOT_ID_LENGTH || !boot_id_is_valid(value))
        return fail("test boot identifier is invalid");
    memcpy(output, value, BOOT_ID_LENGTH + 1);
    return 0;
#else
    unsigned char buffer[BOOT_ID_LENGTH + 2];
    size_t offset = 0;
    int fd = -1;
    int result = -1;

    memset(buffer, 0, sizeof(buffer));
    fd = open("/proc/sys/kernel/random/boot_id",
              O_RDONLY | O_NOFOLLOW | O_CLOEXEC);
    if (fd < 0) {
        fail_errno("open kernel boot identifier");
        goto out;
    }
    while (offset < sizeof(buffer)) {
        ssize_t count = read(fd, buffer + offset, sizeof(buffer) - offset);

        if (count < 0 && errno == EINTR)
            continue;
        if (count < 0) {
            fail_errno("read kernel boot identifier");
            goto out;
        }
        if (count == 0)
            break;
        offset += (size_t)count;
    }
    if (offset == BOOT_ID_LENGTH + 1 && buffer[BOOT_ID_LENGTH] == '\n')
        buffer[BOOT_ID_LENGTH] = '\0';
    else if (offset == BOOT_ID_LENGTH)
        buffer[BOOT_ID_LENGTH] = '\0';
    else {
        fail("kernel boot identifier has an invalid length");
        goto out;
    }
    if (!boot_id_is_valid((const char *)buffer)) {
        fail("kernel boot identifier has an invalid format");
        goto out;
    }
    memcpy(output, buffer, BOOT_ID_LENGTH + 1);
    result = 0;

out:
    if (fd >= 0)
        close(fd);
    secure_zero(buffer, sizeof(buffer));
    return result;
#endif
}

static int validate_seed_contents_at(int dirfd, const char *name,
                                     const char *label,
                                     const struct stat *expected,
                                     nlink_t expected_links,
                                     unsigned char *contents_out);

static int constant_time_equal(const unsigned char *left,
                               const unsigned char *right, size_t length)
{
    unsigned char difference = 0;
    size_t index;

    for (index = 0; index < length; index++)
        difference |= left[index] ^ right[index];
    return difference == 0;
}

static int lowercase_hex_is_valid(const unsigned char *value, size_t length)
{
    size_t index;

    for (index = 0; index < length; index++) {
        if (!((value[index] >= '0' && value[index] <= '9') ||
              (value[index] >= 'a' && value[index] <= 'f')))
            return 0;
    }
    return 1;
}

/*
 * Return 1 when a valid seed-bound marker exists, 0 when absent, and -1 on
 * error.  The marker is exactly "v1 boot-id sha256(seed)" with no newline.
 */
static int read_birth_marker_at(int dirfd, const char *name,
                                const char *seed_name,
                                nlink_t expected_seed_links,
                                char output[BOOT_ID_LENGTH + 1])
{
    unsigned char marker[BIRTH_MARKER_SIZE];
    unsigned char seed[SEED_SIZE];
    unsigned char digest[SHA256_DIGEST_SIZE];
    char digest_hex[SHA256_HEX_SIZE + 1];
    struct stat before_st;
    struct stat after_st;
    unsigned char extra;
    int fd = -1;
    int present;
    int result = -1;

    memset(marker, 0, sizeof(marker));
    memset(seed, 0, sizeof(seed));
    memset(digest, 0, sizeof(digest));
    memset(digest_hex, 0, sizeof(digest_hex));
    present = stat_entry(dirfd, name, &before_st);
    if (present <= 0)
        goto absent_or_error;
    if (!S_ISREG(before_st.st_mode) || !owner_is_trusted(before_st.st_uid) ||
        (before_st.st_mode & 0777) != 0600 ||
        before_st.st_size != BIRTH_MARKER_SIZE || before_st.st_nlink != 1) {
        fail("seed birth marker has unsafe metadata");
        goto out;
    }

    fd = openat(dirfd, name, O_RDONLY | O_NOFOLLOW | O_NOATIME | O_CLOEXEC);
    if (fd < 0) {
        fail_errno("open seed birth marker");
        goto out;
    }
    if (read_exact(fd, marker, sizeof(marker)) != 0) {
        fail_errno("read seed birth marker");
        goto out;
    }
    do {
        present = (int)read(fd, &extra, 1);
    } while (present < 0 && errno == EINTR);
    if (present != 0) {
        fail("seed birth marker has trailing data");
        goto out;
    }
    if (fstat(fd, &after_st) != 0 || !same_inode(&before_st, &after_st) ||
        before_st.st_mtime != after_st.st_mtime ||
        before_st.st_ctime != after_st.st_ctime) {
        fail("seed birth marker changed while it was read");
        goto out;
    }
    if (memcmp(marker, BIRTH_MARKER_VERSION,
               BIRTH_MARKER_VERSION_SIZE) != 0 ||
        marker[BIRTH_MARKER_VERSION_SIZE] != ' ') {
        fail("seed birth marker has an unsupported format version");
        goto out;
    }
    memcpy(output, marker + BIRTH_MARKER_BOOT_OFFSET, BOOT_ID_LENGTH);
    output[BOOT_ID_LENGTH] = '\0';
    if (!boot_id_is_valid(output) ||
        marker[BIRTH_MARKER_DIGEST_OFFSET - 1] != ' ') {
        fail("seed birth marker has an invalid boot identifier");
        goto out;
    }
    if (!lowercase_hex_is_valid(marker + BIRTH_MARKER_DIGEST_OFFSET,
                                SHA256_HEX_SIZE)) {
        fail("seed birth marker has an invalid seed digest");
        goto out;
    }
    if (validate_seed_contents_at(dirfd, seed_name,
                                  "seed bound to birth marker", NULL,
                                  expected_seed_links, seed) != 0)
        goto out;
    sha256_buffer(seed, sizeof(seed), digest);
    sha256_hex(digest, digest_hex);
    if (!constant_time_equal(marker + BIRTH_MARKER_DIGEST_OFFSET,
                             (const unsigned char *)digest_hex,
                             SHA256_HEX_SIZE)) {
        fail("seed birth marker does not match the seed payload");
        goto out;
    }
    result = 1;

out:
    if (fd >= 0)
        close(fd);
    if (result < 0)
        secure_zero(output, BOOT_ID_LENGTH + 1);
    secure_zero(marker, sizeof(marker));
    secure_zero(seed, sizeof(seed));
    secure_zero(digest, sizeof(digest));
    secure_zero(digest_hex, sizeof(digest_hex));
    return result;

absent_or_error:
    result = present;
    goto out;
}

static int create_birth_marker(int dirfd, const char *name,
                               const char *temporary_name,
                               const char *seed_name,
                               nlink_t expected_seed_links)
{
    char boot_id[BOOT_ID_LENGTH + 1];
    unsigned char marker[BIRTH_MARKER_SIZE];
    unsigned char seed[SEED_SIZE];
    unsigned char digest[SHA256_DIGEST_SIZE];
    char digest_hex[SHA256_HEX_SIZE + 1];
    struct stat st;
    int present;
    int fd = -1;
    int result = -1;

    memset(boot_id, 0, sizeof(boot_id));
    memset(marker, 0, sizeof(marker));
    memset(seed, 0, sizeof(seed));
    memset(digest, 0, sizeof(digest));
    memset(digest_hex, 0, sizeof(digest_hex));
    present = stat_entry(dirfd, name, &st);
    if (present < 0)
        goto out;
    if (present != 0) {
        fail("seed birth marker already exists");
        goto out;
    }
    present = stat_entry(dirfd, temporary_name, &st);
    if (present < 0)
        goto out;
    if (present != 0) {
        if (unlinkat(dirfd, temporary_name, 0) != 0) {
            fail_errno("remove stale seed birth-marker temporary");
            goto out;
        }
        if (sync_directory(dirfd,
                           "sync stale seed birth-marker temporary removal") != 0)
            goto out;
    }
    if (read_current_boot_id(boot_id) != 0)
        goto out;
    if (validate_seed_contents_at(dirfd, seed_name,
                                  "seed for birth marker", NULL,
                                  expected_seed_links, seed) != 0)
        goto out;
    sha256_buffer(seed, sizeof(seed), digest);
    sha256_hex(digest, digest_hex);
    memcpy(marker, BIRTH_MARKER_VERSION, BIRTH_MARKER_VERSION_SIZE);
    marker[BIRTH_MARKER_VERSION_SIZE] = ' ';
    memcpy(marker + BIRTH_MARKER_BOOT_OFFSET, boot_id, BOOT_ID_LENGTH);
    marker[BIRTH_MARKER_DIGEST_OFFSET - 1] = ' ';
    memcpy(marker + BIRTH_MARKER_DIGEST_OFFSET, digest_hex, SHA256_HEX_SIZE);
    fd = openat(dirfd, temporary_name,
                O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0600);
    if (fd < 0) {
        fail_errno("create seed birth-marker temporary");
        goto out;
    }
    if (fchmod(fd, 0600) != 0 ||
        write_exact(fd, marker, sizeof(marker)) != 0 ||
        sync_seed_file(fd, "birth-marker") != 0) {
        fail_errno("persist seed birth-marker temporary");
        goto out;
    }
    if (fstat(fd, &st) != 0 || !S_ISREG(st.st_mode) ||
        !owner_is_trusted(st.st_uid) || (st.st_mode & 0777) != 0600 ||
        st.st_size != BIRTH_MARKER_SIZE || st.st_nlink != 1) {
        fail("seed birth-marker temporary failed metadata validation");
        goto out;
    }
    if (close(fd) != 0) {
        fd = -1;
        fail_errno("close seed birth-marker temporary");
        goto out;
    }
    fd = -1;
    test_failpoint("after_birth_marker_temporary_fsync");
    if (renameat(dirfd, temporary_name, dirfd, name) != 0) {
        fail_errno("atomically install seed birth marker");
        goto out;
    }
    test_failpoint("after_birth_marker_install");
    if (sync_directory(dirfd, "sync seed birth marker") != 0)
        goto out;
    test_failpoint("after_birth_marker_fsync");
    if (read_birth_marker_at(dirfd, name, seed_name, expected_seed_links,
                             boot_id) != 1)
        goto out;
    result = 0;

out:
    if (fd >= 0)
        close(fd);
    secure_zero(boot_id, sizeof(boot_id));
    secure_zero(marker, sizeof(marker));
    secure_zero(seed, sizeof(seed));
    secure_zero(digest, sizeof(digest));
    secure_zero(digest_hex, sizeof(digest_hex));
    return result;
}

/*
 * Return 1 when credit must be deferred in this boot, 0 when it may proceed,
 * 2 for an unproven markerless legacy seed, and -1 on error.
 */
static int seed_credit_deferred(int dirfd, const char *birth_name,
                                const char *seed_name)
{
    char born_boot[BOOT_ID_LENGTH + 1];
    char current_boot[BOOT_ID_LENGTH + 1];
    int present;
    int result = -1;

    memset(born_boot, 0, sizeof(born_boot));
    memset(current_boot, 0, sizeof(current_boot));
    present = read_birth_marker_at(dirfd, birth_name, seed_name, 1, born_boot);
    if (present < 0)
        goto out;
    if (present == 0) {
        result = 2;
        goto out;
    }
    if (read_current_boot_id(current_boot) != 0)
        goto out;
    if (memcmp(born_boot, current_boot, BOOT_ID_LENGTH) == 0) {
        result = 1;
        goto out;
    }
    result = 0;

out:
    secure_zero(born_boot, sizeof(born_boot));
    secure_zero(current_boot, sizeof(current_boot));
    return result;
}

/* Catch common erased/zero-filled NAND corruption; this is not an entropy test. */
static int buffer_is_constant(const unsigned char *buffer, size_t length)
{
    size_t index;

    for (index = 1; index < length; index++) {
        if (buffer[index] != buffer[0])
            return 0;
    }
    return 1;
}

static int validate_seed_contents_at(int dirfd, const char *name,
                                     const char *label,
                                     const struct stat *expected,
                                     nlink_t expected_links,
                                     unsigned char *contents_out)
{
    unsigned char buffer[SEED_SIZE];
    struct stat before_st;
    struct stat after_st;
    int fd = -1;
    int result = -1;

    memset(buffer, 0, sizeof(buffer));
    fd = openat(dirfd, name,
                O_RDONLY | O_NOFOLLOW | O_NOATIME | O_CLOEXEC);
    if (fd < 0) {
        fail_errno("open seed for content validation");
        goto out;
    }
    if (fstat(fd, &before_st) != 0) {
        fail_errno("stat seed for content validation");
        goto out;
    }
    if (validate_seed_stat(&before_st, label, expected_links) != 0)
        goto out;
    if (expected != NULL && !same_inode(&before_st, expected)) {
        fail("%s identity changed before content validation", label);
        goto out;
    }
    if (read_exact(fd, buffer, sizeof(buffer)) != 0) {
        fail_errno("read seed for content validation");
        goto out;
    }
    if (fstat(fd, &after_st) != 0) {
        fail_errno("restat seed after content validation");
        goto out;
    }
    if (!same_inode(&before_st, &after_st) ||
        before_st.st_size != after_st.st_size ||
        before_st.st_mtime != after_st.st_mtime ||
        before_st.st_ctime != after_st.st_ctime ||
        validate_seed_stat(&after_st, label, expected_links) != 0) {
        fail("%s changed during content validation", label);
        goto out;
    }
    if (buffer_is_constant(buffer, sizeof(buffer))) {
        fail("%s has an obviously degenerate payload", label);
        goto out;
    }
    if (contents_out != NULL)
        memcpy(contents_out, buffer, sizeof(buffer));
    result = 0;

out:
    if (fd >= 0)
        close(fd);
    secure_zero(buffer, sizeof(buffer));
    return result;
}

/*
 * A credited seed proves that entropy credit is complete.  A successor that
 * has only the private temporary name was never made public and carries no
 * recovery authority, so it can be discarded and regenerated without replay.
 */
static int discard_uninstalled_successor(int dirfd, const char *temporary_name)
{
    if (unlinkat(dirfd, temporary_name, 0) != 0)
        return fail_errno("discard uninstalled successor");
    if (sync_directory(dirfd, "sync uninstalled-successor removal") != 0)
        return -1;
    test_failpoint("after_uninstalled_successor_removal");
    return 0;
}

/*
 * Finish only crash states whose durable credited marker or same-inode
 * temporary link proves that replay is unnecessary.  Return 1 only for a
 * credited seed that still needs a CRNG-ready successor, 2 after cleaning an
 * installed successor, 3 when no lifecycle state exists, 0 for a normal
 * public seed, and -1 for every ambiguous or invalid state.
 */
static int recover_install_state(int dirfd, const char *seed_name,
                                 const char *consumed_name,
                                 const char *credited_name,
                                 const char *birth_name,
                                 const char *birth_temporary_name,
                                 const char *temporary_name)
{
    char birth_boot[BOOT_ID_LENGTH + 1];
    struct stat seed_st;
    struct stat consumed_st;
    struct stat credited_st;
    struct stat birth_st;
    struct stat birth_temporary_st;
    struct stat temporary_st;
    int have_seed = stat_entry(dirfd, seed_name, &seed_st);
    int have_consumed;
    int have_credited;
    int have_birth;
    int have_birth_temporary;
    int have_temporary;
    int recovered_successor = 0;
    int remove_credited = 0;

    memset(birth_boot, 0, sizeof(birth_boot));

    if (have_seed < 0)
        return -1;
    have_consumed = stat_entry(dirfd, consumed_name, &consumed_st);
    if (have_consumed < 0)
        return -1;
    have_credited = stat_entry(dirfd, credited_name, &credited_st);
    if (have_credited < 0)
        return -1;
    have_birth = stat_entry(dirfd, birth_name, &birth_st);
    if (have_birth < 0)
        return -1;
    have_birth_temporary = stat_entry(dirfd, birth_temporary_name,
                                      &birth_temporary_st);
    if (have_birth_temporary < 0)
        return -1;
    have_temporary = stat_entry(dirfd, temporary_name, &temporary_st);
    if (have_temporary < 0)
        return -1;

    if (have_consumed) {
        if (validate_seed_stat(&consumed_st, "consumed seed", 1) != 0)
            return -1;
        return fail("consumed seed state has an ambiguous credit outcome");
    }
    if (have_birth && !have_seed)
        return fail("seed birth marker exists without a public seed");
    if (have_birth && have_birth_temporary)
        return fail("seed birth marker and its transaction temporary coexist");
    if (have_birth_temporary && !(have_seed && have_temporary))
        return fail("orphan seed birth-marker transaction is ambiguous");
    if (!have_seed && !have_credited && !have_birth &&
        !have_birth_temporary && !have_temporary)
        return 3;

    if (have_credited) {
        if (validate_seed_stat(&credited_st, "credited seed", 1) != 0 ||
            validate_seed_contents_at(dirfd, credited_name, "credited seed",
                                      &credited_st, 1, NULL) != 0)
            return -1;
        if (!have_seed && !have_temporary)
            return 1;
        if (!have_seed && have_temporary) {
            if (discard_uninstalled_successor(dirfd, temporary_name) != 0)
                return -1;
            return 1;
        }
        if (!have_seed || !have_temporary)
            return fail("ambiguous credited seed replacement state");
        if (validate_seed_stat(&seed_st, "replacement seed", 2) != 0 ||
            validate_seed_stat(&temporary_st, "replacement witness", 2) != 0)
            return -1;
        if (!same_inode(&seed_st, &temporary_st))
            return fail("replacement seed does not match its install witness");
        recovered_successor = 1;
        remove_credited = 1;
    } else if (have_temporary) {
        if (!have_seed)
            return fail("orphan replacement witness is ambiguous");
        if (validate_seed_stat(&seed_st, "replacement seed", 2) != 0 ||
            validate_seed_stat(&temporary_st, "replacement witness", 2) != 0)
            return -1;
        if (!same_inode(&seed_st, &temporary_st))
            return fail("replacement seed does not match its install witness");
        recovered_successor = 1;
    }

    /*
     * Preserve D/T recovery authority until a durable boot epoch protects the
     * successor.  Otherwise a process crash here could leave a same-boot
     * successor looking like a legacy public seed and authorize circular
     * entropy credit on retry.
     */
    if (recovered_successor) {
        if (have_birth) {
            if (read_birth_marker_at(dirfd, birth_name, seed_name, 2,
                                     birth_boot) != 1)
                return -1;
        } else if (create_birth_marker(dirfd, birth_name,
                                       birth_temporary_name, seed_name,
                                       2) != 0) {
            return -1;
        }
    }

    if (remove_credited) {
        if (unlinkat(dirfd, credited_name, 0) != 0)
            return fail_errno("remove credited seed after proven replacement");
        if (sync_directory(dirfd, "sync credited-seed removal") != 0)
            return -1;
        test_failpoint("after_recovery_credited_removal");
    }

    if (have_temporary) {
        if (unlinkat(dirfd, temporary_name, 0) != 0)
            return fail_errno("remove replacement witness");
        if (sync_directory(dirfd, "sync replacement-witness removal") != 0)
            return -1;
        test_failpoint("after_recovery_witness_removal");
    }

    have_seed = stat_entry(dirfd, seed_name, &seed_st);
    if (have_seed <= 0)
        return have_seed < 0 ? -1 : fail("seed file is absent");
    if (validate_seed_stat(&seed_st, "seed file", 1) != 0)
        return -1;
    secure_zero(birth_boot, sizeof(birth_boot));
    return recovered_successor ? 2 : 0;
}

/*
 * Create a first seed during orderly shutdown without touching an existing
 * seed.  The helper itself proves kernel random-pool readiness before reading
 * seed material, so correctness does not depend on caller timing.
 */
static int initialize_seed_if_missing(int dirfd, const char *seed_name,
                                      const char *consumed_name,
                                      const char *credited_name,
                                      const char *birth_name,
                                      const char *birth_temporary_name,
                                      const char *temporary_name)
{
    char birth_boot[BOOT_ID_LENGTH + 1];
    unsigned char new_seed[SEED_SIZE];
    unsigned char credited_seed[SEED_SIZE];
    struct stat seed_st;
    struct stat consumed_st;
    struct stat credited_st;
    struct stat birth_st;
    struct stat birth_temporary_st;
    struct stat temporary_st;
    int have_seed;
    int have_consumed;
    int have_credited;
    int have_birth;
    int have_birth_temporary;
    int have_temporary;
    int resume_credited = 0;
    int randomfd = -1;
    int temporaryfd = -1;
    int result = -1;

    memset(new_seed, 0, sizeof(new_seed));
    memset(credited_seed, 0, sizeof(credited_seed));
    memset(birth_boot, 0, sizeof(birth_boot));

    have_seed = stat_entry(dirfd, seed_name, &seed_st);
    if (have_seed < 0)
        goto out;
    have_consumed = stat_entry(dirfd, consumed_name, &consumed_st);
    if (have_consumed < 0)
        goto out;
    have_credited = stat_entry(dirfd, credited_name, &credited_st);
    if (have_credited < 0)
        goto out;
    have_birth = stat_entry(dirfd, birth_name, &birth_st);
    if (have_birth < 0)
        goto out;
    have_birth_temporary = stat_entry(dirfd, birth_temporary_name,
                                      &birth_temporary_st);
    if (have_birth_temporary < 0)
        goto out;
    have_temporary = stat_entry(dirfd, temporary_name, &temporary_st);
    if (have_temporary < 0)
        goto out;

    if (have_consumed && have_credited) {
        fail("consumed and credited seed states coexist");
        goto out;
    }
    if (have_consumed) {
        if (validate_seed_stat(&consumed_st, "consumed seed", 1) != 0)
            goto out;
        fail("consumed seed state requires boot-time lifecycle resolution");
        goto out;
    }
    if (have_birth && !have_seed) {
        fail("seed birth marker exists without a public seed");
        goto out;
    }
    if (have_birth && have_birth_temporary) {
        fail("seed birth marker and its transaction temporary coexist");
        goto out;
    }
    if (have_birth_temporary && !(have_seed && have_temporary)) {
        fail("orphan seed birth-marker transaction is ambiguous");
        goto out;
    }

    if (have_credited) {
        if (validate_seed_stat(&credited_st, "credited seed", 1) != 0 ||
            validate_seed_contents_at(dirfd, credited_name, "credited seed",
                                      &credited_st, 1, credited_seed) != 0)
            goto out;
        if (have_seed) {
            fail("credited replacement state requires boot-time lifecycle resolution");
            goto out;
        }
        if (have_temporary &&
            discard_uninstalled_successor(dirfd, temporary_name) != 0)
            goto out;
        resume_credited = 1;
    }

    if (have_temporary && !resume_credited) {
        if (!have_seed) {
            if (validate_seed_stat(&temporary_st, "orphan seed witness", 1) != 0)
                goto out;
            fail("orphan seed witness is ambiguous");
            goto out;
        }
        if (validate_seed_stat(&seed_st, "initialized seed", 2) != 0 ||
            validate_seed_stat(&temporary_st, "initialization witness", 2) != 0)
            goto out;
        if (!same_inode(&seed_st, &temporary_st)) {
            fail("initialized seed does not match its install witness");
            goto out;
        }
        if (validate_seed_contents_at(dirfd, seed_name, "initialized seed",
                                      &seed_st, 2, NULL) != 0)
            goto out;
        if (have_birth) {
            if (read_birth_marker_at(dirfd, birth_name, seed_name, 2,
                                     birth_boot) != 1)
                goto out;
        } else if (create_birth_marker(dirfd, birth_name,
                                       birth_temporary_name, seed_name,
                                       2) != 0) {
            goto out;
        }

        if (unlinkat(dirfd, temporary_name, 0) != 0) {
            fail_errno("remove initialization witness");
            goto out;
        }
        if (sync_directory(dirfd, "sync initialization-witness removal") != 0)
            goto out;
        if (fstatat(dirfd, seed_name, &seed_st,
                    AT_SYMLINK_NOFOLLOW) != 0) {
            fail_errno("verify recovered initialized seed");
            goto out;
        }
        if (validate_seed_stat(&seed_st, "recovered initialized seed", 1) != 0)
            goto out;
        if (validate_seed_contents_at(dirfd, seed_name,
                                      "recovered initialized seed",
                                      &seed_st, 1, NULL) != 0)
            goto out;
        result = 0;
        goto out;
    }

    if (have_seed && !resume_credited) {
        if (validate_seed_stat(&seed_st, "existing seed", 1) != 0)
            goto out;
        if (validate_seed_contents_at(dirfd, seed_name, "existing seed",
                                      &seed_st, 1, NULL) != 0)
            goto out;
        if (have_birth &&
            read_birth_marker_at(dirfd, birth_name, seed_name, 1,
                                 birth_boot) != 1)
            goto out;
        result = 0;
        goto out;
    }

    /* Either every state is absent or a durable credited seed needs a successor. */
    randomfd = open_random_device(O_RDONLY);
    if (randomfd < 0 || validate_random_fd(randomfd) != 0) {
        if (randomfd < 0)
            fail_errno("open /dev/urandom for seed initialization");
        goto out;
    }
    if (random_source_ready(randomfd) != 0)
        goto out;
    if (read_exact(randomfd, new_seed, sizeof(new_seed)) != 0) {
        fail_errno("read initial seed");
        goto out;
    }
    close(randomfd);
    randomfd = -1;
    if (buffer_is_constant(new_seed, sizeof(new_seed))) {
        fail("initial seed source returned an obviously degenerate payload");
        goto out;
    }
    if (resume_credited &&
        memcmp(new_seed, credited_seed, sizeof(new_seed)) == 0) {
        fail("successor source repeated the credited seed");
        goto out;
    }

    temporaryfd = openat(dirfd, temporary_name,
                         O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC,
                         0600);
    if (temporaryfd < 0) {
        fail_errno("create initial seed");
        goto out;
    }
    if (fchmod(temporaryfd, 0600) != 0 ||
        write_exact(temporaryfd, new_seed, sizeof(new_seed)) != 0 ||
        sync_seed_file(temporaryfd, "successor") != 0) {
        fail_errno("persist initial seed");
        goto out;
    }
    if (fstat(temporaryfd, &temporary_st) != 0) {
        fail_errno("stat initial seed");
        goto out;
    }
    if (validate_seed_stat(&temporary_st, "initial seed", 1) != 0)
        goto out;
    if (close(temporaryfd) != 0) {
        temporaryfd = -1;
        fail_errno("close initial seed");
        goto out;
    }
    temporaryfd = -1;
    secure_zero(new_seed, sizeof(new_seed));
    test_failpoint("after_replacement_fsync");

    if (linkat(dirfd, temporary_name, dirfd, seed_name, 0) != 0) {
        fail_errno("atomically install initial seed");
        goto out;
    }
    test_failpoint("after_install_link");
    if (sync_directory(dirfd, "sync initial seed install") != 0)
        goto out;
    test_failpoint("after_install_fsync");

    if (create_birth_marker(dirfd, birth_name, birth_temporary_name,
                            seed_name, 2) != 0)
        goto out;

    if (resume_credited) {
        if (unlinkat(dirfd, credited_name, 0) != 0) {
            fail_errno("remove credited seed after initialization recovery");
            goto out;
        }
        if (sync_directory(dirfd, "sync initialization credited-seed removal") != 0)
            goto out;
        test_failpoint("after_initialize_credited_removal");
    }

    if (unlinkat(dirfd, temporary_name, 0) != 0) {
        fail_errno("remove initialization witness");
        goto out;
    }
    test_failpoint("after_initialize_witness_unlink");
    if (sync_directory(dirfd, "sync initialization-witness removal") != 0)
        goto out;

    if (fstatat(dirfd, seed_name, &seed_st,
                AT_SYMLINK_NOFOLLOW) != 0) {
        fail_errno("verify initialized seed");
        goto out;
    }
    if (validate_seed_stat(&seed_st, "initialized seed", 1) != 0)
        goto out;
    result = 0;

out:
    if (temporaryfd >= 0)
        close(temporaryfd);
    if (randomfd >= 0)
        close(randomfd);
    secure_zero(new_seed, sizeof(new_seed));
    secure_zero(credited_seed, sizeof(credited_seed));
    secure_zero(birth_boot, sizeof(birth_boot));
    return result;
}

/*
 * Older DCENT_OS releases wrote markerless /dev/urandom output during
 * shutdown without proving CRNG readiness.  Such bytes remain useful as
 * no-credit input but must never authorize entropy accounting.  Remove the
 * public seed durably before mixing it so no crash can later reinterpret it
 * as creditable state, then create a fresh seed only after getrandom() proves
 * the current CRNG is initialized.
 */
static int migrate_untrusted_seed(int dirfd, const char *seed_name,
                                  const char *consumed_name,
                                  const char *credited_name,
                                  const char *birth_name,
                                  const char *birth_temporary_name,
                                  const char *temporary_name)
{
    unsigned char seed[SEED_SIZE];
    struct stat seed_st;
    int randomfd = -1;
    int result = -1;

    memset(seed, 0, sizeof(seed));
    if (fstatat(dirfd, seed_name, &seed_st, AT_SYMLINK_NOFOLLOW) != 0) {
        fail_errno("stat untrusted legacy seed");
        goto out;
    }
    if (validate_seed_contents_at(dirfd, seed_name,
                                  "untrusted legacy seed", &seed_st, 1,
                                  seed) != 0)
        goto out;

    randomfd = open_random_device(O_WRONLY);
    if (randomfd < 0 || validate_random_fd(randomfd) != 0) {
        if (randomfd < 0)
            fail_errno("open /dev/urandom for legacy seed mixing");
        goto out;
    }
    if (unlinkat(dirfd, seed_name, 0) != 0) {
        fail_errno("remove untrusted legacy seed");
        goto out;
    }
    if (sync_directory(dirfd, "sync untrusted legacy-seed removal") != 0)
        goto out;
    test_failpoint("after_untrusted_seed_removal");

    if (mix_seed_without_credit(randomfd, seed) != 0) {
        fail_errno("mix untrusted legacy seed without credit");
        goto out;
    }
    close(randomfd);
    randomfd = -1;
    test_failpoint("after_untrusted_seed_mix");
    secure_zero(seed, sizeof(seed));

    result = initialize_seed_if_missing(dirfd, seed_name, consumed_name,
                                        credited_name, birth_name,
                                        birth_temporary_name,
                                        temporary_name);

out:
    if (randomfd >= 0)
        close(randomfd);
    secure_zero(seed, sizeof(seed));
    return result;
}

int main(int argc, char **argv)
{
    const char *seed_path = NULL;
    int initialize_mode = 0;
    int initialize_fd_mode = 0;
    int supplied_dirfd = -1;
    char seed_name[NAME_MAX + 1];
    char consumed_name[NAME_MAX + 1];
    char credited_name[NAME_MAX + 1];
    char birth_name[NAME_MAX + 1];
    char birth_temporary_name[NAME_MAX + 1];
    char temporary_name[NAME_MAX + 1];
    unsigned char old_seed[SEED_SIZE];
    unsigned char new_seed[SEED_SIZE];
    struct fixed_rand_pool_info pool;
    struct stat before_st;
    struct stat after_st;
    struct stat installed_st;
    int dirfd = -1;
    int seedfd = -1;
    int randomfd = -1;
    int temporaryfd = -1;
    int result = 1;
    int recovery_state;
    int credit_deferred;
    int count;

    memset(old_seed, 0, sizeof(old_seed));
    memset(new_seed, 0, sizeof(new_seed));
    memset(&pool, 0, sizeof(pool));

    if (argc == 4 && strcmp(argv[1], "--initialize-if-missing-at") == 0) {
        char *end = NULL;
        long parsed_fd;

        errno = 0;
        parsed_fd = strtol(argv[2], &end, 10);
        if (errno != 0 || end == argv[2] || *end != '\0' || parsed_fd < 3 ||
            parsed_fd > INT_MAX) {
            fail("seed directory descriptor is invalid");
            goto out;
        }
        if (copy_safe_basename(argv[3], seed_name, sizeof(seed_name)) != 0)
            goto out;
        supplied_dirfd = (int)parsed_fd;
        initialize_mode = 1;
        initialize_fd_mode = 1;
    } else if (argc == 3 && strcmp(argv[1], "--initialize-if-missing") == 0) {
        initialize_mode = 1;
        seed_path = argv[2];
    } else if (argc <= 2 &&
               (argc == 1 || strcmp(argv[1], "--initialize-if-missing") != 0)) {
        seed_path = argc == 2 ? argv[1] : default_seed;
    } else {
        fail("usage: seed-entropy [absolute-seed-file] | "
             "--initialize-if-missing absolute-seed-file | "
             "--initialize-if-missing-at directory-fd seed-basename");
        goto out;
    }
    if (sizeof(int) != 4) {
        fail("kernel random ioctl ABI requires 32-bit int fields");
        goto out;
    }
    if (sha256_self_test() != 0) {
        fail("internal SHA-256 self-test failed");
        goto out;
    }
    if (initialize_fd_mode)
        dirfd = open_secure_directory_fd(supplied_dirfd);
    else
        dirfd = open_secure_parent(seed_path, seed_name, sizeof(seed_name));
    if (dirfd < 0)
        goto out;

    count = snprintf(consumed_name, sizeof(consumed_name), ".%s.consumed",
                     seed_name);
    if (count < 0 || (size_t)count >= sizeof(consumed_name)) {
        fail("seed basename is too long for consumed state");
        goto out;
    }
    count = snprintf(credited_name, sizeof(credited_name), ".%s.credited",
                     seed_name);
    if (count < 0 || (size_t)count >= sizeof(credited_name)) {
        fail("seed basename is too long for credited state");
        goto out;
    }
    count = snprintf(birth_name, sizeof(birth_name), ".%s.born", seed_name);
    if (count < 0 || (size_t)count >= sizeof(birth_name)) {
        fail("seed basename is too long for birth state");
        goto out;
    }
    count = snprintf(birth_temporary_name, sizeof(birth_temporary_name),
                     ".%s.born.new", seed_name);
    if (count < 0 || (size_t)count >= sizeof(birth_temporary_name)) {
        fail("seed basename is too long for birth-marker transaction state");
        goto out;
    }
    count = snprintf(temporary_name, sizeof(temporary_name), ".%s.new",
                     seed_name);
    if (count < 0 || (size_t)count >= sizeof(temporary_name)) {
        fail("seed basename is too long for replacement state");
        goto out;
    }

    if (initialize_mode) {
        result = initialize_seed_if_missing(dirfd, seed_name, consumed_name,
                                            credited_name, birth_name,
                                            birth_temporary_name,
                                            temporary_name) == 0 ? 0 : 1;
        goto out;
    }

    recovery_state = recover_install_state(dirfd, seed_name, consumed_name,
                                           credited_name, birth_name,
                                           birth_temporary_name,
                                           temporary_name);
    if (recovery_state < 0)
        goto out;
    if (recovery_state == 1) {
        if (validate_seed_contents_at(dirfd, credited_name, "credited seed",
                                      NULL, 1, old_seed) != 0)
            goto out;
        goto generate_replacement;
    }
    if (recovery_state == 3) {
        result = initialize_seed_if_missing(dirfd, seed_name, consumed_name,
                                            credited_name, birth_name,
                                            birth_temporary_name,
                                            temporary_name) == 0 ? 0 : 1;
        goto out;
    }

    credit_deferred = seed_credit_deferred(dirfd, birth_name, seed_name);
    if (credit_deferred < 0)
        goto out;
    if (credit_deferred == 1) {
        result = 0;
        goto out;
    }
    if (credit_deferred == 2) {
        result = migrate_untrusted_seed(dirfd, seed_name, consumed_name,
                                        credited_name, birth_name,
                                        birth_temporary_name,
                                        temporary_name) == 0 ? 0 : 1;
        goto out;
    }

    seedfd = openat(dirfd, seed_name,
                    O_RDONLY | O_NOFOLLOW | O_NOATIME | O_CLOEXEC);
    if (seedfd < 0) {
        fail_errno("open seed file");
        goto out;
    }
    if (fstat(seedfd, &before_st) != 0) {
        fail_errno("stat open seed file");
        goto out;
    }
    if (validate_seed_stat(&before_st, "open seed file", 1) != 0)
        goto out;
    if (read_exact(seedfd, old_seed, sizeof(old_seed)) != 0) {
        fail_errno("read seed file");
        goto out;
    }
    if (buffer_is_constant(old_seed, sizeof(old_seed))) {
        fail("seed file has an obviously degenerate payload");
        goto out;
    }
    if (fstat(seedfd, &after_st) != 0) {
        fail_errno("restat open seed file");
        goto out;
    }
    if (!same_inode(&before_st, &after_st) ||
        before_st.st_size != after_st.st_size ||
        before_st.st_mtime != after_st.st_mtime ||
        before_st.st_ctime != after_st.st_ctime ||
        validate_seed_stat(&after_st, "read seed file", 1) != 0) {
        fail("seed file changed while it was read");
        goto out;
    }

    randomfd = open_random_device(O_WRONLY);
    if (randomfd < 0 || validate_random_fd(randomfd) != 0) {
        if (randomfd < 0)
            fail_errno("open /dev/urandom for entropy restoration");
        goto out;
    }

    if (unlinkat(dirfd, birth_name, 0) != 0) {
        fail_errno("remove expired seed birth marker");
        goto out;
    }
    if (sync_directory(dirfd, "sync expired birth-marker removal") != 0)
        goto out;
    test_failpoint("after_expired_birth_marker_removal");

    if (renameat(dirfd, seed_name, dirfd, consumed_name) != 0) {
        fail_errno("atomically consume seed file");
        goto out;
    }
    if (fstatat(dirfd, consumed_name, &installed_st,
                AT_SYMLINK_NOFOLLOW) != 0 ||
        !same_inode(&before_st, &installed_st) ||
        validate_seed_stat(&installed_st, "consumed seed", 1) != 0) {
        fail("consumed seed identity changed during rename");
        goto out;
    }
    if (sync_directory(dirfd, "sync consumed seed") != 0)
        goto out;
    test_failpoint("after_consume_fsync");

    close(seedfd);
    seedfd = -1;

    pool.entropy_count = CREDIT_BITS;
    pool.buf_size = SEED_SIZE;
    memcpy(pool.buf, old_seed, sizeof(old_seed));

    if (mix_seed_without_credit(randomfd, old_seed) != 0) {
        fail_errno("mix saved seed without credit");
        goto out;
    }
    if (add_entropy(randomfd, &pool) != 0) {
        fail_errno("RNDADDENTROPY");
        goto out;
    }
    close(randomfd);
    randomfd = -1;
    secure_zero(&pool, sizeof(pool));
    test_failpoint("after_ioctl");

    if (renameat(dirfd, consumed_name, dirfd, credited_name) != 0) {
        fail_errno("mark seed credit as durable");
        goto out;
    }
    if (fstatat(dirfd, credited_name, &installed_st,
                AT_SYMLINK_NOFOLLOW) != 0 ||
        !same_inode(&before_st, &installed_st) ||
        validate_seed_stat(&installed_st, "credited seed", 1) != 0) {
        fail("credited seed identity changed during marker rename");
        goto out;
    }
    if (sync_directory(dirfd, "sync credited seed") != 0)
        goto out;
    test_failpoint("after_credit_marker_fsync");

generate_replacement:
    /* Reading successor bytes requires both credit proof and CRNG readiness. */
    randomfd = open_random_device(O_RDONLY);
    if (randomfd < 0 || validate_random_fd(randomfd) != 0) {
        if (randomfd < 0)
            fail_errno("open /dev/urandom for replacement seed");
        goto out;
    }
    if (random_source_ready(randomfd) != 0)
        goto out;
    if (read_exact(randomfd, new_seed, sizeof(new_seed)) != 0) {
        fail_errno("read replacement seed");
        goto out;
    }
    close(randomfd);
    randomfd = -1;

    if (memcmp(new_seed, old_seed, sizeof(new_seed)) == 0) {
        fail("replacement source repeated the consumed seed");
        goto out;
    }
    if (buffer_is_constant(new_seed, sizeof(new_seed))) {
        fail("replacement source returned an obviously degenerate payload");
        goto out;
    }
    secure_zero(old_seed, sizeof(old_seed));

    temporaryfd = openat(dirfd, temporary_name,
                         O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC,
                         0600);
    if (temporaryfd < 0) {
        fail_errno("create replacement seed");
        goto out;
    }
    if (fchmod(temporaryfd, 0600) != 0 ||
        write_exact(temporaryfd, new_seed, sizeof(new_seed)) != 0 ||
        sync_seed_file(temporaryfd, "successor") != 0) {
        fail_errno("persist replacement seed");
        goto out;
    }
    if (fstat(temporaryfd, &installed_st) != 0) {
        fail_errno("stat replacement seed");
        goto out;
    }
    if (validate_seed_stat(&installed_st, "replacement seed", 1) != 0)
        goto out;
    if (close(temporaryfd) != 0) {
        temporaryfd = -1;
        fail_errno("close replacement seed");
        goto out;
    }
    temporaryfd = -1;
    secure_zero(new_seed, sizeof(new_seed));
    test_failpoint("after_replacement_fsync");

    if (linkat(dirfd, temporary_name, dirfd, seed_name, 0) != 0) {
        fail_errno("atomically install replacement seed");
        goto out;
    }
    test_failpoint("after_install_link");
    if (sync_directory(dirfd, "sync replacement seed install") != 0)
        goto out;
    test_failpoint("after_install_fsync");

    if (create_birth_marker(dirfd, birth_name, birth_temporary_name,
                            seed_name, 2) != 0)
        goto out;

    /*
     * Keep the same-inode witness until credited-seed removal is durable.
     * Restart will only trust an interrupted install while that proof exists.
     */
    if (unlinkat(dirfd, credited_name, 0) != 0) {
        fail_errno("remove credited seed");
        goto out;
    }
    if (sync_directory(dirfd, "sync credited seed removal") != 0)
        goto out;
    test_failpoint("after_credited_removal");

    if (unlinkat(dirfd, temporary_name, 0) != 0) {
        fail_errno("remove replacement witness");
        goto out;
    }
    if (sync_directory(dirfd, "sync replacement witness removal") != 0)
        goto out;

    if (fstatat(dirfd, seed_name, &installed_st,
                AT_SYMLINK_NOFOLLOW) != 0 ||
        validate_seed_stat(&installed_st, "installed seed", 1) != 0) {
        fail("installed seed failed final verification");
        goto out;
    }

    result = 0;

out:
    if (temporaryfd >= 0)
        close(temporaryfd);
    if (randomfd >= 0)
        close(randomfd);
    if (seedfd >= 0)
        close(seedfd);
    if (dirfd >= 0)
        close(dirfd);
    secure_zero(&pool, sizeof(pool));
    secure_zero(old_seed, sizeof(old_seed));
    secure_zero(new_seed, sizeof(new_seed));
    return result;
}
