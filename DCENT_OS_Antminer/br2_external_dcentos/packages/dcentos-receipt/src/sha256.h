/* SPDX-License-Identifier: LicenseRef-Public-Domain */
#ifndef DCENTOS_RECEIPT_SHA256_H
#define DCENTOS_RECEIPT_SHA256_H

#include <stddef.h>
#include <stdint.h>

#define DCENT_RECEIPT_SHA256_BYTES 32U
#define DCENT_RECEIPT_SHA256_HEX_BYTES 64U

void dcent_receipt_sha256(uint8_t output[DCENT_RECEIPT_SHA256_BYTES],
                          const void *input, size_t input_length);
void dcent_receipt_sha256_hex(
    char output[DCENT_RECEIPT_SHA256_HEX_BYTES + 1U],
    const uint8_t digest[DCENT_RECEIPT_SHA256_BYTES]);

#endif
