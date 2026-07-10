/*
 * seed-entropy — Credit saved entropy to the kernel random pool.
 *
 * Writing to /dev/urandom does NOT credit entropy bits. The kernel
 * still considers the pool uninitialized until ~200s of hardware
 * entropy accumulates (on Zynq ARM with no hwrng).
 *
 * This uses the RNDADDENTROPY ioctl to credit bits from a saved
 * seed file, making the random pool immediately available for SSH.
 *
 * CRITICAL: This is a C binary, NOT Python. Python3 reads /dev/urandom
 * during interpreter startup (hash randomization), which drains entropy
 * before the ioctl can credit it. This binary opens /dev/urandom
 * O_WRONLY only — no reads, no entropy drain.
 *
 * Usage: seed-entropy [seed-file]
 *   Default seed file: /data/keys/random-seed
 *
 * D-Central Technologies — DCENTos
 */

#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/ioctl.h>
#include <unistd.h>

/* Linux random ioctl: add entropy and credit bits */
#define RNDADDENTROPY 0x40085203

/* Conservative: credit 256 bits from 512 bytes of saved seed */
#define CREDIT_BITS 256
#define MIN_SEED_SIZE 64
#define MAX_SEED_SIZE 4096

static const char *DEFAULT_SEED = "/data/keys/random-seed";

int main(int argc, char *argv[])
{
    const char *seed_file = argc > 1 ? argv[1] : DEFAULT_SEED;
    int fd_seed, fd_urandom;
    ssize_t seed_size;
    unsigned char seed_buf[MAX_SEED_SIZE];

    /* Read the seed file */
    fd_seed = open(seed_file, O_RDONLY);
    if (fd_seed < 0)
        return 0; /* No seed file — not an error, first boot */

    seed_size = read(fd_seed, seed_buf, MAX_SEED_SIZE);
    close(fd_seed);

    if (seed_size < MIN_SEED_SIZE)
        return 0; /* Too small to be useful */

    /*
     * Build the rand_pool_info structure:
     *   int entropy_count  — bits to credit
     *   int buf_size       — size of entropy data
     *   char buf[]         — the entropy data
     *
     * We allocate this as a flat buffer to avoid flexible array issues.
     */
    {
        unsigned char iobuf[sizeof(int) * 2 + MAX_SEED_SIZE];
        int *header = (int *)iobuf;

        header[0] = CREDIT_BITS;    /* entropy_count */
        header[1] = (int)seed_size; /* buf_size */

        /* Copy seed data after the header */
        int i;
        for (i = 0; i < seed_size; i++)
            iobuf[sizeof(int) * 2 + i] = seed_buf[i];

        /* Open /dev/urandom for WRITING only — no reads, no entropy drain */
        fd_urandom = open("/dev/urandom", O_WRONLY);
        if (fd_urandom < 0)
            return 1;

        ioctl(fd_urandom, RNDADDENTROPY, iobuf);
        close(fd_urandom);
    }

    return 0;
}
