/* SPDX-License-Identifier: GPL-3.0-or-later */
#include "receipt_storage.h"

#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define FRAME_FIELD_BYTES 6U
#define FRAME_HEADER_BYTES (3U * FRAME_FIELD_BYTES)
#define FRAME_MAX_BYTES                                                       \
    (FRAME_HEADER_BYTES + DCENT_RECEIPT_STORAGE_MAX_SEAL +                    \
     (2U * DCENT_RECEIPT_STORAGE_MAX_HEAD))

struct framed_records {
    const uint8_t *seal;
    size_t seal_size;
    const uint8_t *head0;
    size_t head0_size;
    const uint8_t *head1;
    size_t head1_size;
};

static int parse_length(const uint8_t *field, size_t *out)
{
    size_t index;
    size_t value = 0U;

    if (field[FRAME_FIELD_BYTES - 1U] != '\n')
        return 0;
    for (index = 0U; index < FRAME_FIELD_BYTES - 1U; index++) {
        if (field[index] < '0' || field[index] > '9')
            return 0;
        value = (value * 10U) + (size_t)(field[index] - '0');
    }
    *out = value;
    return 1;
}

static int parse_frame(const uint8_t *data, size_t size,
                       struct framed_records *records)
{
    size_t payload_size;

    if (data == NULL || records == NULL || size < FRAME_HEADER_BYTES ||
        size > FRAME_MAX_BYTES ||
        !parse_length(data, &records->seal_size) ||
        !parse_length(data + FRAME_FIELD_BYTES, &records->head0_size) ||
        !parse_length(data + (2U * FRAME_FIELD_BYTES),
                      &records->head1_size) ||
        records->seal_size > DCENT_RECEIPT_STORAGE_MAX_SEAL ||
        records->head0_size > DCENT_RECEIPT_STORAGE_MAX_HEAD ||
        records->head1_size > DCENT_RECEIPT_STORAGE_MAX_HEAD)
        return 0;
    payload_size = records->seal_size + records->head0_size;
    if (payload_size < records->seal_size ||
        payload_size > size - FRAME_HEADER_BYTES ||
        records->head1_size != size - FRAME_HEADER_BYTES - payload_size)
        return 0;
    records->seal = data + FRAME_HEADER_BYTES;
    records->head0 = records->seal + records->seal_size;
    records->head1 = records->head0 + records->head0_size;
    return 1;
}

static void require_failure_atomic(
    enum dcent_receipt_storage_result result, const void *before,
    const void *after, size_t size)
{
    if (result != DCENT_RECEIPT_STORAGE_OK &&
        memcmp(before, after, size) != 0)
        abort();
}

static int exercise_records(const void *seal_bytes, size_t seal_size,
                            const void *head0_bytes, size_t head0_size,
                            const void *head1_bytes, size_t head1_size,
                            int require_valid)
{
    struct dcent_receipt_storage_seal seal;
    struct dcent_receipt_storage_seal seal_before;
    struct dcent_receipt_storage_head head0;
    struct dcent_receipt_storage_head head0_before;
    struct dcent_receipt_storage_head head1;
    struct dcent_receipt_storage_head head1_before;
    struct dcent_receipt_storage_manifest_pair pair;
    struct dcent_receipt_storage_manifest_pair pair_before;
    enum dcent_receipt_storage_result seal_result;
    enum dcent_receipt_storage_result head0_result;
    enum dcent_receipt_storage_result head1_result;
    enum dcent_receipt_storage_result pair_result;

    memset(&seal, 0xa5, sizeof(seal));
    seal_before = seal;
    seal_result = dcent_receipt_storage_parse_seal_abi2(
        seal_bytes, seal_size, &seal);
    require_failure_atomic(seal_result, &seal_before, &seal, sizeof(seal));

    memset(&head0, 0x5a, sizeof(head0));
    head0_before = head0;
    head0_result = dcent_receipt_storage_parse_head_abi2(
        head0_bytes, head0_size, &head0);
    require_failure_atomic(head0_result, &head0_before, &head0,
                           sizeof(head0));

    memset(&head1, 0x3c, sizeof(head1));
    head1_before = head1;
    head1_result = dcent_receipt_storage_parse_head_abi2(
        head1_bytes, head1_size, &head1);
    require_failure_atomic(head1_result, &head1_before, &head1,
                           sizeof(head1));

    if (seal_result != DCENT_RECEIPT_STORAGE_OK ||
        head0_result != DCENT_RECEIPT_STORAGE_OK ||
        head1_result != DCENT_RECEIPT_STORAGE_OK) {
        if (require_valid)
            abort();
        return 0;
    }

    memset(&pair, 0xc3, sizeof(pair));
    pair_before = pair;
    pair_result = dcent_receipt_storage_validate_manifest_pair_abi2(
        &seal, &head0, &head1, &pair);
    require_failure_atomic(pair_result, &pair_before, &pair, sizeof(pair));
    if (pair_result == DCENT_RECEIPT_STORAGE_OK && !pair.initialized)
        abort();
    if (require_valid &&
        (pair_result != DCENT_RECEIPT_STORAGE_OK || !pair.genesis ||
         pair.current_generation != 0U || pair.current_bank != 0U))
        abort();
    return pair_result == DCENT_RECEIPT_STORAGE_OK;
}

static void exercise_individual(const uint8_t *data, size_t size)
{
    struct dcent_receipt_storage_seal seal;
    struct dcent_receipt_storage_seal seal_before;
    struct dcent_receipt_storage_head head;
    struct dcent_receipt_storage_head head_before;
    enum dcent_receipt_storage_result result;

    memset(&seal, 0xa5, sizeof(seal));
    seal_before = seal;
    result = dcent_receipt_storage_parse_seal_abi2(data, size, &seal);
    require_failure_atomic(result, &seal_before, &seal, sizeof(seal));
    memset(&head, 0x5a, sizeof(head));
    head_before = head;
    result = dcent_receipt_storage_parse_head_abi2(data, size, &head);
    require_failure_atomic(result, &head_before, &head, sizeof(head));
}

int LLVMFuzzerTestOneInput(const uint8_t *data, size_t size)
{
    struct framed_records records;

    exercise_individual(data, size);
    if (parse_frame(data, size, &records))
        (void)exercise_records(records.seal, records.seal_size, records.head0,
                               records.head0_size, records.head1,
                               records.head1_size, 0);
    return 0;
}

#ifdef DCENT_RECEIPT_STORAGE_FUZZ_CORPUS_MAIN
int main(int argc, char **argv)
{
    struct framed_records records;
    uint8_t *bytes;
    FILE *input;
    long length;
    size_t size;
    int valid;

    if (argc != 2)
        return 2;
    input = fopen(argv[1], "rb");
    if (input == NULL || fseek(input, 0L, SEEK_END) != 0)
        return 2;
    length = ftell(input);
    if (length < 0 || (unsigned long)length > FRAME_MAX_BYTES ||
        fseek(input, 0L, SEEK_SET) != 0) {
        fclose(input);
        return 2;
    }
    size = (size_t)length;
    bytes = malloc(size == 0U ? 1U : size);
    if (bytes == NULL || fread(bytes, 1U, size, input) != size) {
        free(bytes);
        fclose(input);
        return 2;
    }
    fclose(input);
    valid = parse_frame(bytes, size, &records) &&
            exercise_records(records.seal, records.seal_size, records.head0,
                             records.head0_size, records.head1,
                             records.head1_size, 1);
    free(bytes);
    return valid ? 0 : 1;
}
#endif
