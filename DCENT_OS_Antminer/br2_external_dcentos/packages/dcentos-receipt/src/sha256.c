/* SPDX-License-Identifier: LicenseRef-Public-Domain */
/*
 * Public-domain FIPS-180 SHA-256 implementation adapted from util-linux
 * 2.41.1, include/sha256.h and lib/sha256.c.  The upstream files state that
 * no copyright is claimed and the code is in the public domain.
 *
 * DCENT_OS changes: namespace every symbol, accept size_t input, use explicit
 * portable initialization, expose lowercase hex encoding, and format for the
 * project.  This bounded one-shot primitive avoids a provider/config/dynamic
 * ABI dependency in the privileged recovery receipt path.
 */

#include "sha256.h"

#include <stdint.h>
#include <string.h>

struct dcent_sha256_state {
    uint64_t length;
    uint32_t hash[8];
    uint8_t buffer[64];
};

static uint32_t rotate_right(uint32_t value, unsigned int bits)
{
    return (value >> bits) | (value << (32U - bits));
}

#define CHOOSE(x, y, z) ((z) ^ ((x) & ((y) ^ (z))))
#define MAJORITY(x, y, z) (((x) & (y)) | ((z) & ((x) | (y))))
#define BIG_SIGMA_0(x) \
    (rotate_right((x), 2U) ^ rotate_right((x), 13U) ^ \
     rotate_right((x), 22U))
#define BIG_SIGMA_1(x) \
    (rotate_right((x), 6U) ^ rotate_right((x), 11U) ^ \
     rotate_right((x), 25U))
#define SMALL_SIGMA_0(x) \
    (rotate_right((x), 7U) ^ rotate_right((x), 18U) ^ ((x) >> 3U))
#define SMALL_SIGMA_1(x) \
    (rotate_right((x), 17U) ^ rotate_right((x), 19U) ^ ((x) >> 10U))

static const uint32_t round_constants[64] = {
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
    0x90befffaU, 0xa4506cebU, 0xbef9a3f7U, 0xc67178f2U,
};

static void process_block(struct dcent_sha256_state *state,
                          const uint8_t block[64])
{
    uint32_t words[64];
    uint32_t temporary_1;
    uint32_t temporary_2;
    uint32_t a;
    uint32_t b;
    uint32_t c;
    uint32_t d;
    uint32_t e;
    uint32_t f;
    uint32_t g;
    uint32_t h;
    unsigned int index;

    for (index = 0; index < 16U; ++index) {
        words[index] = (uint32_t)block[4U * index] << 24U;
        words[index] |= (uint32_t)block[4U * index + 1U] << 16U;
        words[index] |= (uint32_t)block[4U * index + 2U] << 8U;
        words[index] |= block[4U * index + 3U];
    }
    for (; index < 64U; ++index) {
        words[index] = SMALL_SIGMA_1(words[index - 2U]) +
                       words[index - 7U] +
                       SMALL_SIGMA_0(words[index - 15U]) +
                       words[index - 16U];
    }

    a = state->hash[0];
    b = state->hash[1];
    c = state->hash[2];
    d = state->hash[3];
    e = state->hash[4];
    f = state->hash[5];
    g = state->hash[6];
    h = state->hash[7];
    for (index = 0; index < 64U; ++index) {
        temporary_1 = h + BIG_SIGMA_1(e) + CHOOSE(e, f, g) +
                      round_constants[index] + words[index];
        temporary_2 = BIG_SIGMA_0(a) + MAJORITY(a, b, c);
        h = g;
        g = f;
        f = e;
        e = d + temporary_1;
        d = c;
        c = b;
        b = a;
        a = temporary_1 + temporary_2;
    }
    state->hash[0] += a;
    state->hash[1] += b;
    state->hash[2] += c;
    state->hash[3] += d;
    state->hash[4] += e;
    state->hash[5] += f;
    state->hash[6] += g;
    state->hash[7] += h;
}

static void initialize(struct dcent_sha256_state *state)
{
    memset(state, 0, sizeof(*state));
    state->hash[0] = 0x6a09e667U;
    state->hash[1] = 0xbb67ae85U;
    state->hash[2] = 0x3c6ef372U;
    state->hash[3] = 0xa54ff53aU;
    state->hash[4] = 0x510e527fU;
    state->hash[5] = 0x9b05688cU;
    state->hash[6] = 0x1f83d9abU;
    state->hash[7] = 0x5be0cd19U;
}

static void update(struct dcent_sha256_state *state, const uint8_t *input,
                   size_t input_length)
{
    size_t buffered = (size_t)(state->length % 64U);

    state->length += (uint64_t)input_length;
    if (buffered != 0U) {
        size_t available = 64U - buffered;

        if (input_length < available) {
            memcpy(state->buffer + buffered, input, input_length);
            return;
        }
        memcpy(state->buffer + buffered, input, available);
        input += available;
        input_length -= available;
        process_block(state, state->buffer);
    }
    while (input_length >= 64U) {
        process_block(state, input);
        input += 64U;
        input_length -= 64U;
    }
    if (input_length != 0U)
        memcpy(state->buffer, input, input_length);
}

static void finish(struct dcent_sha256_state *state,
                   uint8_t output[DCENT_RECEIPT_SHA256_BYTES])
{
    size_t buffered = (size_t)(state->length % 64U);
    uint64_t bit_length;
    unsigned int index;

    state->buffer[buffered++] = 0x80U;
    if (buffered > 56U) {
        memset(state->buffer + buffered, 0, 64U - buffered);
        process_block(state, state->buffer);
        buffered = 0;
    }
    memset(state->buffer + buffered, 0, 56U - buffered);
    bit_length = state->length * 8U;
    for (index = 0; index < 8U; ++index) {
        state->buffer[63U - index] = (uint8_t)bit_length;
        bit_length >>= 8U;
    }
    process_block(state, state->buffer);

    for (index = 0; index < 8U; ++index) {
        output[4U * index] = (uint8_t)(state->hash[index] >> 24U);
        output[4U * index + 1U] =
            (uint8_t)(state->hash[index] >> 16U);
        output[4U * index + 2U] =
            (uint8_t)(state->hash[index] >> 8U);
        output[4U * index + 3U] = (uint8_t)state->hash[index];
    }
}

void dcent_receipt_sha256(uint8_t output[DCENT_RECEIPT_SHA256_BYTES],
                          const void *input, size_t input_length)
{
    struct dcent_sha256_state state;

    initialize(&state);
    update(&state, (const uint8_t *)input, input_length);
    finish(&state, output);
}

void dcent_receipt_sha256_hex(
    char output[DCENT_RECEIPT_SHA256_HEX_BYTES + 1U],
    const uint8_t digest[DCENT_RECEIPT_SHA256_BYTES])
{
    static const char hex[] = "0123456789abcdef";
    unsigned int index;

    for (index = 0; index < DCENT_RECEIPT_SHA256_BYTES; ++index) {
        output[2U * index] = hex[digest[index] >> 4U];
        output[2U * index + 1U] = hex[digest[index] & 0x0fU];
    }
    output[DCENT_RECEIPT_SHA256_HEX_BYTES] = '\0';
}
