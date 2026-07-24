/* SPDX-License-Identifier: GPL-3.0-or-later */
#include "receipt_storage.h"

#include <limits.h>
#include <string.h>

struct storage_cursor {
    const unsigned char *data;
    size_t size;
    size_t offset;
};

struct storage_value {
    const unsigned char *data;
    size_t size;
};

static bool bytes_equal(const unsigned char *left, const unsigned char *right,
                        size_t size)
{
    return memcmp(left, right, size) == 0;
}

static bool value_equal_text(const struct storage_value *value,
                             const char *text)
{
    size_t size = strlen(text);

    return value->size == size && memcmp(value->data, text, size) == 0;
}

static bool canonical_ascii(const unsigned char *data, size_t size)
{
    size_t index;

    if (size == 0U || data[size - 1U] != '\n')
        return false;
    for (index = 0U; index < size; index++) {
        if (data[index] == '\n')
            continue;
        if (data[index] < 0x20U || data[index] > 0x7eU)
            return false;
    }
    return true;
}

static bool next_line(struct storage_cursor *cursor,
                      struct storage_value *line)
{
    const unsigned char *end;
    size_t remaining;

    if (cursor->offset >= cursor->size)
        return false;
    remaining = cursor->size - cursor->offset;
    end = memchr(cursor->data + cursor->offset, '\n', remaining);
    if (end == NULL || end == cursor->data + cursor->offset)
        return false;
    line->data = cursor->data + cursor->offset;
    line->size = (size_t)(end - line->data);
    cursor->offset += line->size + 1U;
    return true;
}

static bool next_field(struct storage_cursor *cursor, const char *key,
                       struct storage_value *value)
{
    struct storage_value line;
    size_t key_size = strlen(key);

    if (!next_line(cursor, &line) || line.size <= key_size ||
        memcmp(line.data, key, key_size) != 0 || line.data[key_size] != '=')
        return false;
    value->data = line.data + key_size + 1U;
    value->size = line.size - key_size - 1U;
    return value->size != 0U;
}

static bool id_from_value(const struct storage_value *value,
                          struct dcent_receipt_id *out)
{
    struct dcent_receipt_id parsed;
    size_t index;

    if (value->size == 0U || value->size > DCENT_RECEIPT_MAX_ID)
        return false;
    if (!((value->data[0] >= 'A' && value->data[0] <= 'Z') ||
          (value->data[0] >= 'a' && value->data[0] <= 'z') ||
          (value->data[0] >= '0' && value->data[0] <= '9')))
        return false;
    for (index = 1U; index < value->size; index++) {
        unsigned char byte = value->data[index];

        if (!((byte >= 'A' && byte <= 'Z') ||
              (byte >= 'a' && byte <= 'z') ||
              (byte >= '0' && byte <= '9') || byte == '.' || byte == '_' ||
              byte == '-'))
            return false;
    }
    memset(&parsed, 0, sizeof(parsed));
    parsed.size = value->size;
    memcpy(parsed.bytes, value->data, value->size);
    *out = parsed;
    return true;
}

static bool uuid_from_value(const struct storage_value *value,
                            struct dcent_receipt_id *out)
{
    static const size_t hyphens[] = {8U, 13U, 18U, 23U};
    size_t index;
    size_t hyphen_index = 0U;

    if (value->size != 36U)
        return false;
    for (index = 0U; index < value->size; index++) {
        if (hyphen_index < sizeof(hyphens) / sizeof(hyphens[0]) &&
            index == hyphens[hyphen_index]) {
            if (value->data[index] != '-')
                return false;
            hyphen_index++;
        } else if (!((value->data[index] >= '0' &&
                      value->data[index] <= '9') ||
                     (value->data[index] >= 'a' &&
                      value->data[index] <= 'f'))) {
            return false;
        }
    }
    return id_from_value(value, out);
}

static bool uint64_from_value(const struct storage_value *value,
                              uint64_t *out)
{
    uint64_t parsed = 0U;
    size_t index;

    if (value->size == 0U ||
        (value->size > 1U && value->data[0] == '0'))
        return false;
    for (index = 0U; index < value->size; index++) {
        unsigned int digit;

        if (value->data[index] < '0' || value->data[index] > '9')
            return false;
        digit = (unsigned int)(value->data[index] - '0');
        if (parsed > (UINT64_MAX - digit) / 10U)
            return false;
        parsed = parsed * 10U + digit;
    }
    *out = parsed;
    return true;
}

static bool digest_from_value(const struct storage_value *value,
                              unsigned char out[DCENT_RECEIPT_SHA256_BYTES])
{
    unsigned char parsed[DCENT_RECEIPT_SHA256_BYTES];
    size_t index;

    if (value->size != DCENT_RECEIPT_SHA256_HEX_BYTES)
        return false;
    for (index = 0U; index < DCENT_RECEIPT_SHA256_BYTES; index++) {
        unsigned char high = value->data[index * 2U];
        unsigned char low = value->data[index * 2U + 1U];
        unsigned int high_value;
        unsigned int low_value;

        if (high >= '0' && high <= '9')
            high_value = (unsigned int)(high - '0');
        else if (high >= 'a' && high <= 'f')
            high_value = (unsigned int)(high - 'a') + 10U;
        else
            return false;
        if (low >= '0' && low <= '9')
            low_value = (unsigned int)(low - '0');
        else if (low >= 'a' && low <= 'f')
            low_value = (unsigned int)(low - 'a') + 10U;
        else
            return false;
        parsed[index] = (unsigned char)((high_value << 4U) | low_value);
    }
    memcpy(out, parsed, sizeof(parsed));
    return true;
}

static bool devino_from_value(const struct storage_value *value,
                              struct dcent_receipt_devino *out)
{
    struct storage_value device;
    struct storage_value inode;
    struct dcent_receipt_devino parsed;
    const unsigned char *colon;

    colon = memchr(value->data, ':', value->size);
    if (colon == NULL || colon == value->data ||
        colon == value->data + value->size - 1U ||
        memchr(colon + 1, ':',
               value->size - (size_t)(colon + 1 - value->data)) != NULL)
        return false;
    device.data = value->data;
    device.size = (size_t)(colon - value->data);
    inode.data = colon + 1;
    inode.size = value->size - device.size - 1U;
    if (!uint64_from_value(&device, &parsed.device) ||
        !uint64_from_value(&inode, &parsed.inode))
        return false;
    *out = parsed;
    return true;
}

static bool resource_kind_from_value(
    const struct storage_value *value,
    enum dcent_receipt_resource_kind *out)
{
    if (value_equal_text(value, "attachment"))
        *out = DCENT_RECEIPT_KIND_ATTACHMENT;
    else if (value_equal_text(value, "node"))
        *out = DCENT_RECEIPT_KIND_NODE;
    else if (value_equal_text(value, "mount"))
        *out = DCENT_RECEIPT_KIND_MOUNT;
    else if (value_equal_text(value, "workspace"))
        *out = DCENT_RECEIPT_KIND_WORKSPACE;
    else
        return false;
    return true;
}

static bool transaction_phase_from_value(
    const struct storage_value *value, enum dcent_receipt_lock_phase *out)
{
    if (value_equal_text(value, "active"))
        *out = DCENT_RECEIPT_LOCK_ACTIVE;
    else if (value_equal_text(value, "cleanup-required"))
        *out = DCENT_RECEIPT_LOCK_CLEANUP_REQUIRED;
    else if (value_equal_text(value, "env-commit-armed"))
        *out = DCENT_RECEIPT_LOCK_ENV_COMMIT_ARMED;
    else if (value_equal_text(value, "env-committed"))
        *out = DCENT_RECEIPT_LOCK_ENV_COMMITTED;
    else
        return false;
    return true;
}

static int id_compare(const struct dcent_receipt_id *left,
                      const struct dcent_receipt_id *right)
{
    size_t common = left->size < right->size ? left->size : right->size;
    int result = memcmp(left->bytes, right->bytes, common);

    if (result != 0)
        return result;
    if (left->size < right->size)
        return -1;
    if (left->size > right->size)
        return 1;
    return 0;
}

static bool row_before(const struct dcent_receipt_storage_resource_head *left,
                       const struct dcent_receipt_storage_resource_head *right)
{
    if (left->kind != right->kind)
        return left->kind < right->kind;
    return id_compare(&left->resource_id, &right->resource_id) < 0;
}

static bool parse_resource_row(
    const struct storage_value *value,
    struct dcent_receipt_storage_resource_head *out)
{
    struct dcent_receipt_storage_resource_head parsed;
    struct storage_value fields[5];
    size_t field_index;
    size_t start = 0U;
    uint64_t revision;

    memset(&parsed, 0, sizeof(parsed));
    for (field_index = 0U; field_index < 4U; field_index++) {
        const unsigned char *colon =
            memchr(value->data + start, ':', value->size - start);

        if (colon == NULL || colon == value->data + start)
            return false;
        fields[field_index].data = value->data + start;
        fields[field_index].size =
            (size_t)(colon - (value->data + start));
        start = (size_t)(colon - value->data) + 1U;
    }
    if (start >= value->size ||
        memchr(value->data + start, ':', value->size - start) != NULL)
        return false;
    fields[4].data = value->data + start;
    fields[4].size = value->size - start;

    if (!resource_kind_from_value(&fields[0], &parsed.kind) ||
        !id_from_value(&fields[1], &parsed.resource_id) ||
        !digest_from_value(&fields[2], parsed.intent_sha256) ||
        !uint64_from_value(&fields[3], &revision) || revision == 0U ||
        revision > DCENT_RECEIPT_MAX_REVISIONS ||
        !digest_from_value(&fields[4], parsed.status_sha256))
        return false;
    parsed.status_revision = (uint32_t)revision;
    *out = parsed;
    return true;
}

enum dcent_receipt_storage_result dcent_receipt_storage_parse_seal_abi2(
    const void *data, size_t size, struct dcent_receipt_storage_seal *out)
{
    struct dcent_receipt_storage_seal parsed;
    struct storage_cursor cursor;
    struct storage_value value;
    uint64_t number;

    if (data == NULL || out == NULL)
        return DCENT_RECEIPT_STORAGE_INVALID_ARGUMENT;
    if (size > DCENT_RECEIPT_STORAGE_MAX_SEAL)
        return DCENT_RECEIPT_STORAGE_LIMIT;
    if (!canonical_ascii(data, size))
        return DCENT_RECEIPT_STORAGE_MALFORMED;

    memset(&parsed, 0, sizeof(parsed));
    cursor.data = data;
    cursor.size = size;
    cursor.offset = 0U;
    if (!next_field(&cursor, "schema", &value) ||
        !value_equal_text(&value,
                          "dcentos-sysupgrade-ledger-seal-abi2") ||
        !next_field(&cursor, "transaction_id", &value) ||
        !id_from_value(&value, &parsed.transaction_id) ||
        !next_field(&cursor, "boot_id", &value) ||
        !uuid_from_value(&value, &parsed.boot_id) ||
        !next_field(&cursor, "acquisition_guard_device_inode", &value) ||
        !devino_from_value(&value,
                           &parsed.acquisition_guard_device_inode) ||
        !next_field(&cursor, "transaction_lock_device_inode", &value) ||
        !devino_from_value(&value,
                           &parsed.transaction_lock_device_inode) ||
        !next_field(&cursor, "transaction_lock_owner_device_inode", &value) ||
        !devino_from_value(&value,
                           &parsed.transaction_lock_owner_device_inode) ||
        !next_field(&cursor, "transaction_lock_owner_sha256", &value) ||
         !digest_from_value(&value,
                            parsed.transaction_lock_owner_sha256) ||
         !next_field(&cursor, "storage_mount_id", &value) ||
         !uint64_from_value(&value, &number) || number == 0U ||
        !next_field(&cursor, "ledger_device_inode", &value) ||
        !devino_from_value(&value, &parsed.ledger_device_inode) ||
        !next_field(&cursor, "binding_sha256", &value) ||
        !digest_from_value(&value, parsed.binding_sha256) ||
        !next_field(&cursor, "mutation_lease_device_inode", &value) ||
        !devino_from_value(&value, &parsed.mutation_lease_device_inode) ||
        !next_field(&cursor, "initial_transaction_phase", &value) ||
        !value_equal_text(&value, "active") ||
        !next_field(&cursor, "owner", &value) ||
        !value_equal_text(&value, "zynq-sysupgrade") ||
        cursor.offset != cursor.size)
        return DCENT_RECEIPT_STORAGE_MALFORMED;

    parsed.storage_mount_id = number;
    parsed.initialized = true;
    dcent_receipt_sha256(parsed.record_sha256, data, size);
    *out = parsed;
    return DCENT_RECEIPT_STORAGE_OK;
}

enum dcent_receipt_storage_result dcent_receipt_storage_parse_head_abi2(
    const void *data, size_t size, struct dcent_receipt_storage_head *out)
{
    struct dcent_receipt_storage_head parsed;
    struct storage_cursor cursor;
    struct storage_value value;
    uint64_t number;
    uint64_t revision_sum = 0U;
    size_t index;

    if (data == NULL || out == NULL)
        return DCENT_RECEIPT_STORAGE_INVALID_ARGUMENT;
    if (size > DCENT_RECEIPT_STORAGE_MAX_HEAD)
        return DCENT_RECEIPT_STORAGE_LIMIT;
    if (!canonical_ascii(data, size))
        return DCENT_RECEIPT_STORAGE_MALFORMED;

    memset(&parsed, 0, sizeof(parsed));
    cursor.data = data;
    cursor.size = size;
    cursor.offset = 0U;
    if (!next_field(&cursor, "schema", &value) ||
        !value_equal_text(&value,
                          "dcentos-sysupgrade-ledger-head-abi2") ||
        !next_field(&cursor, "seal_sha256", &value) ||
        !digest_from_value(&value, parsed.seal_sha256) ||
        !next_field(&cursor, "generation", &value) ||
        !uint64_from_value(&value, &number))
        return DCENT_RECEIPT_STORAGE_MALFORMED;
    if (number > DCENT_RECEIPT_STORAGE_MAX_GENERATION)
        return DCENT_RECEIPT_STORAGE_LIMIT;
    parsed.generation = (uint32_t)number;

    if (!next_field(&cursor, "previous_generation", &value))
        return DCENT_RECEIPT_STORAGE_MALFORMED;
    if (value_equal_text(&value, "-")) {
        parsed.previous_present = false;
    } else {
        if (!uint64_from_value(&value, &number) || number > UINT32_MAX)
            return DCENT_RECEIPT_STORAGE_MALFORMED;
        parsed.previous_present = true;
        parsed.previous_generation = (uint32_t)number;
    }
    if (!next_field(&cursor, "previous_head_sha256", &value))
        return DCENT_RECEIPT_STORAGE_MALFORMED;
    if (parsed.previous_present) {
        if (!digest_from_value(&value, parsed.previous_head_sha256))
            return DCENT_RECEIPT_STORAGE_MALFORMED;
    } else if (!value_equal_text(&value, "-")) {
        return DCENT_RECEIPT_STORAGE_MALFORMED;
    }
    if ((parsed.generation == 0U && parsed.previous_present) ||
        (parsed.generation != 0U &&
         (!parsed.previous_present ||
          parsed.previous_generation + 1U != parsed.generation)))
        return DCENT_RECEIPT_STORAGE_SEMANTIC;

    if (!next_field(&cursor, "authority_kind", &value))
        return DCENT_RECEIPT_STORAGE_MALFORMED;
    if (value_equal_text(&value, "owner"))
        parsed.authority = DCENT_RECEIPT_AUTHORITY_OWNER;
    else if (value_equal_text(&value, "reconciler"))
        parsed.authority = DCENT_RECEIPT_AUTHORITY_RECONCILER;
    else
        return DCENT_RECEIPT_STORAGE_MALFORMED;
    if (!next_field(&cursor, "authority_id", &value) ||
        !id_from_value(&value, &parsed.authority_id) ||
        !next_field(&cursor, "transaction_phase", &value) ||
        !transaction_phase_from_value(&value, &parsed.transaction_phase) ||
        !next_field(&cursor, "transaction_phase_revision", &value) ||
        !uint64_from_value(&value, &number))
        return DCENT_RECEIPT_STORAGE_MALFORMED;
    if (number > DCENT_RECEIPT_STORAGE_MAX_PHASE_REVISIONS)
        return DCENT_RECEIPT_STORAGE_LIMIT;
    parsed.transaction_phase_revision = (uint32_t)number;
    if (!next_field(&cursor, "transaction_phase_status_sha256", &value))
        return DCENT_RECEIPT_STORAGE_MALFORMED;
    if (parsed.transaction_phase_revision == 0U) {
        if (parsed.transaction_phase != DCENT_RECEIPT_LOCK_ACTIVE ||
            !value_equal_text(&value, "-"))
            return DCENT_RECEIPT_STORAGE_SEMANTIC;
        parsed.transaction_phase_status_present = false;
    } else {
        if (!digest_from_value(&value,
                               parsed.transaction_phase_status_sha256))
            return DCENT_RECEIPT_STORAGE_MALFORMED;
        parsed.transaction_phase_status_present = true;
    }

    if (!next_field(&cursor, "claim_present", &value))
        return DCENT_RECEIPT_STORAGE_MALFORMED;
    if (value_equal_text(&value, "0"))
        parsed.claim_present = false;
    else if (value_equal_text(&value, "1"))
        parsed.claim_present = true;
    else
        return DCENT_RECEIPT_STORAGE_MALFORMED;

    if (!next_field(&cursor, "claim_id", &value))
        return DCENT_RECEIPT_STORAGE_MALFORMED;
    if (parsed.claim_present) {
        if (!id_from_value(&value, &parsed.claim_id))
            return DCENT_RECEIPT_STORAGE_MALFORMED;
    } else if (!value_equal_text(&value, "-")) {
        return DCENT_RECEIPT_STORAGE_MALFORMED;
    }
    if (!next_field(&cursor, "claim_intent_sha256", &value))
        return DCENT_RECEIPT_STORAGE_MALFORMED;
    if (parsed.claim_present) {
        if (!digest_from_value(&value, parsed.claim_intent_sha256))
            return DCENT_RECEIPT_STORAGE_MALFORMED;
    } else if (!value_equal_text(&value, "-")) {
        return DCENT_RECEIPT_STORAGE_MALFORMED;
    }
    if (!next_field(&cursor, "claim_status_revision", &value) ||
        !uint64_from_value(&value, &number))
        return DCENT_RECEIPT_STORAGE_MALFORMED;
    if ((!parsed.claim_present && number != 0U) ||
        (parsed.claim_present &&
         (number == 0U || number > DCENT_RECEIPT_MAX_REVISIONS)))
        return DCENT_RECEIPT_STORAGE_SEMANTIC;
    parsed.claim_status_revision = (uint32_t)number;
    if (!next_field(&cursor, "claim_status_sha256", &value))
        return DCENT_RECEIPT_STORAGE_MALFORMED;
    if (parsed.claim_present) {
        if (!digest_from_value(&value, parsed.claim_status_sha256))
            return DCENT_RECEIPT_STORAGE_MALFORMED;
    } else if (!value_equal_text(&value, "-")) {
        return DCENT_RECEIPT_STORAGE_MALFORMED;
    }
    if ((!parsed.claim_present &&
         parsed.authority != DCENT_RECEIPT_AUTHORITY_OWNER) ||
        (parsed.claim_present &&
         (parsed.authority != DCENT_RECEIPT_AUTHORITY_RECONCILER ||
          id_compare(&parsed.authority_id, &parsed.claim_id) != 0)))
        return DCENT_RECEIPT_STORAGE_SEMANTIC;

    if (!next_field(&cursor, "resource_count", &value) ||
        !uint64_from_value(&value, &number))
        return DCENT_RECEIPT_STORAGE_MALFORMED;
    if (number > DCENT_RECEIPT_MAX_RESOURCES)
        return DCENT_RECEIPT_STORAGE_LIMIT;
    parsed.resource_count = (size_t)number;
    for (index = 0U; index < parsed.resource_count; index++) {
        struct storage_value line;
        unsigned int tens = (unsigned int)(index / 10U);
        unsigned int ones = (unsigned int)(index % 10U);

        if (!next_line(&cursor, &line) || line.size < 13U ||
            memcmp(line.data, "resource.", 9U) != 0 ||
            line.data[9] != (unsigned char)('0' + tens) ||
            line.data[10] != (unsigned char)('0' + ones) ||
            line.data[11] != '=')
            return DCENT_RECEIPT_STORAGE_MALFORMED;
        value.data = line.data + 12U;
        value.size = line.size - 12U;
        if (!parse_resource_row(&value, &parsed.resources[index]))
            return DCENT_RECEIPT_STORAGE_MALFORMED;
        if (index != 0U &&
            !row_before(&parsed.resources[index - 1U],
                        &parsed.resources[index]))
            return DCENT_RECEIPT_STORAGE_SEMANTIC;
        revision_sum += parsed.resources[index].status_revision;
    }
    if (cursor.offset != cursor.size)
        return DCENT_RECEIPT_STORAGE_MALFORMED;
    if (parsed.claim_present)
        revision_sum += parsed.claim_status_revision;
    revision_sum += parsed.transaction_phase_revision;
    if (revision_sum != parsed.generation)
        return DCENT_RECEIPT_STORAGE_SEMANTIC;

    parsed.initialized = true;
    dcent_receipt_sha256(parsed.record_sha256, data, size);
    *out = parsed;
    return DCENT_RECEIPT_STORAGE_OK;
}

static bool resource_head_equal(
    const struct dcent_receipt_storage_resource_head *left,
    const struct dcent_receipt_storage_resource_head *right)
{
    return left->kind == right->kind &&
           id_compare(&left->resource_id, &right->resource_id) == 0 &&
           bytes_equal(left->intent_sha256, right->intent_sha256,
                       DCENT_RECEIPT_SHA256_BYTES) &&
           left->status_revision == right->status_revision &&
           bytes_equal(left->status_sha256, right->status_sha256,
                       DCENT_RECEIPT_SHA256_BYTES);
}

static bool resource_identity_equal(
    const struct dcent_receipt_storage_resource_head *left,
    const struct dcent_receipt_storage_resource_head *right)
{
    return left->kind == right->kind &&
           id_compare(&left->resource_id, &right->resource_id) == 0;
}

static bool authority_equal(const struct dcent_receipt_storage_head *left,
                            const struct dcent_receipt_storage_head *right)
{
    return left->authority == right->authority &&
           id_compare(&left->authority_id, &right->authority_id) == 0;
}

static bool phase_equal(const struct dcent_receipt_storage_head *left,
                        const struct dcent_receipt_storage_head *right)
{
    return left->transaction_phase == right->transaction_phase &&
           left->transaction_phase_revision ==
               right->transaction_phase_revision &&
           left->transaction_phase_status_present ==
               right->transaction_phase_status_present &&
           (!left->transaction_phase_status_present ||
            bytes_equal(left->transaction_phase_status_sha256,
                        right->transaction_phase_status_sha256,
                        DCENT_RECEIPT_SHA256_BYTES));
}

static bool claim_equal(const struct dcent_receipt_storage_head *left,
                        const struct dcent_receipt_storage_head *right)
{
    if (left->claim_present != right->claim_present)
        return false;
    if (!left->claim_present)
        return true;
    return id_compare(&left->claim_id, &right->claim_id) == 0 &&
           bytes_equal(left->claim_intent_sha256,
                       right->claim_intent_sha256,
                       DCENT_RECEIPT_SHA256_BYTES) &&
           left->claim_status_revision == right->claim_status_revision &&
           bytes_equal(left->claim_status_sha256,
                       right->claim_status_sha256,
                       DCENT_RECEIPT_SHA256_BYTES);
}

static bool head_links(const struct dcent_receipt_storage_head *older,
                       const struct dcent_receipt_storage_head *newer)
{
    return newer->generation == older->generation + 1U &&
           newer->previous_present &&
           newer->previous_generation == older->generation &&
           bytes_equal(newer->previous_head_sha256, older->record_sha256,
                       DCENT_RECEIPT_SHA256_BYTES) &&
           bytes_equal(newer->seal_sha256, older->seal_sha256,
                       DCENT_RECEIPT_SHA256_BYTES);
}

static bool all_resources_equal(const struct dcent_receipt_storage_head *left,
                                const struct dcent_receipt_storage_head *right)
{
    size_t index;

    if (left->resource_count != right->resource_count)
        return false;
    for (index = 0U; index < left->resource_count; index++) {
        if (!resource_head_equal(&left->resources[index],
                                 &right->resources[index]))
            return false;
    }
    return true;
}

static bool one_resource_added(
    const struct dcent_receipt_storage_head *older,
    const struct dcent_receipt_storage_head *newer, size_t *added_index)
{
    size_t old_index = 0U;
    size_t new_index = 0U;
    bool found = false;

    if (newer->resource_count != older->resource_count + 1U)
        return false;
    while (new_index < newer->resource_count) {
        if (!found &&
            (old_index == older->resource_count ||
             !resource_identity_equal(&older->resources[old_index],
                                      &newer->resources[new_index]))) {
            found = true;
            *added_index = new_index;
            new_index++;
            continue;
        }
        if (old_index == older->resource_count ||
            !resource_head_equal(&older->resources[old_index],
                                 &newer->resources[new_index]))
            return false;
        old_index++;
        new_index++;
    }
    return found && old_index == older->resource_count;
}

static bool digest_is_zero(
    const unsigned char digest[DCENT_RECEIPT_SHA256_BYTES])
{
    size_t index;

    for (index = 0U; index < DCENT_RECEIPT_SHA256_BYTES; index++) {
        if (digest[index] != 0U)
            return false;
    }
    return true;
}

static bool owned_id_valid(const struct dcent_receipt_id *value)
{
    struct storage_value slice;
    struct dcent_receipt_id ignored;

    if (value == NULL || value->size > DCENT_RECEIPT_MAX_ID)
        return false;
    slice.data = value->bytes;
    slice.size = value->size;
    return id_from_value(&slice, &ignored);
}

static bool head_projection_valid(
    const struct dcent_receipt_storage_seal *seal,
    const struct dcent_receipt_storage_head *head)
{
    uint64_t revision_sum;
    size_t index;

    if (seal == NULL || head == NULL || !seal->initialized ||
        !head->initialized || !owned_id_valid(&seal->transaction_id) ||
        !bytes_equal(head->seal_sha256, seal->record_sha256,
                     DCENT_RECEIPT_SHA256_BYTES) ||
        head->generation > DCENT_RECEIPT_STORAGE_MAX_GENERATION ||
        head->resource_count > DCENT_RECEIPT_MAX_RESOURCES ||
        !owned_id_valid(&head->authority_id))
        return false;
    if ((head->generation == 0U && head->previous_present) ||
        (head->generation != 0U &&
         (!head->previous_present ||
          head->previous_generation + 1U != head->generation)))
        return false;
    if (!head->previous_present &&
        !digest_is_zero(head->previous_head_sha256))
        return false;
    if (head->transaction_phase < DCENT_RECEIPT_LOCK_ACTIVE ||
        head->transaction_phase > DCENT_RECEIPT_LOCK_ENV_COMMITTED ||
        head->transaction_phase_revision >
            DCENT_RECEIPT_STORAGE_MAX_PHASE_REVISIONS)
        return false;
    if (head->transaction_phase_revision == 0U) {
        if (head->transaction_phase != DCENT_RECEIPT_LOCK_ACTIVE ||
            head->transaction_phase_status_present ||
            !digest_is_zero(head->transaction_phase_status_sha256))
            return false;
    } else if (!head->transaction_phase_status_present) {
        return false;
    }
    if (!head->claim_present) {
        if (head->authority != DCENT_RECEIPT_AUTHORITY_OWNER ||
            id_compare(&head->authority_id, &seal->transaction_id) != 0 ||
            head->claim_id.size != 0U ||
            head->claim_status_revision != 0U ||
            !digest_is_zero(head->claim_intent_sha256) ||
            !digest_is_zero(head->claim_status_sha256))
            return false;
    } else if (head->authority != DCENT_RECEIPT_AUTHORITY_RECONCILER ||
               !owned_id_valid(&head->claim_id) ||
               id_compare(&head->authority_id, &head->claim_id) != 0 ||
               head->claim_status_revision == 0U ||
               head->claim_status_revision > DCENT_RECEIPT_MAX_REVISIONS) {
        return false;
    }

    revision_sum = head->transaction_phase_revision;
    if (head->claim_present)
        revision_sum += head->claim_status_revision;
    for (index = 0U; index < head->resource_count; index++) {
        const struct dcent_receipt_storage_resource_head *resource =
            &head->resources[index];

        if (resource->kind < DCENT_RECEIPT_KIND_ATTACHMENT ||
            resource->kind > DCENT_RECEIPT_KIND_WORKSPACE ||
            !owned_id_valid(&resource->resource_id) ||
            resource->status_revision == 0U ||
            resource->status_revision > DCENT_RECEIPT_MAX_REVISIONS ||
            (index != 0U && !row_before(&head->resources[index - 1U],
                                        resource)))
            return false;
        revision_sum += resource->status_revision;
    }
    return revision_sum == head->generation;
}

static bool phase_transition_valid(enum dcent_receipt_lock_phase from,
                                   enum dcent_receipt_lock_phase to)
{
    return (from == DCENT_RECEIPT_LOCK_ACTIVE &&
            (to == DCENT_RECEIPT_LOCK_CLEANUP_REQUIRED ||
             to == DCENT_RECEIPT_LOCK_ENV_COMMIT_ARMED)) ||
           (from == DCENT_RECEIPT_LOCK_ENV_COMMIT_ARMED &&
            (to == DCENT_RECEIPT_LOCK_ACTIVE ||
             to == DCENT_RECEIPT_LOCK_ENV_COMMITTED));
}

static bool resource_mutation_phase_valid(
    enum dcent_receipt_lock_phase phase)
{
    return phase != DCENT_RECEIPT_LOCK_ENV_COMMIT_ARMED &&
           phase != DCENT_RECEIPT_LOCK_ENV_COMMITTED;
}

static bool resource_advance_authority_valid(
    const struct dcent_receipt_storage_head *head)
{
    if (!head->claim_present)
        return head->authority == DCENT_RECEIPT_AUTHORITY_OWNER;
    return head->authority == DCENT_RECEIPT_AUTHORITY_RECONCILER &&
           head->claim_status_revision == 3U;
}

static bool claim_mutation_phase_valid(enum dcent_receipt_lock_phase phase)
{
    return phase == DCENT_RECEIPT_LOCK_ACTIVE ||
           phase == DCENT_RECEIPT_LOCK_CLEANUP_REQUIRED;
}

static bool phase_advance_authority_valid(
    const struct dcent_receipt_storage_head *older,
    const struct dcent_receipt_storage_head *newer)
{
    if (newer->transaction_phase == DCENT_RECEIPT_LOCK_CLEANUP_REQUIRED) {
        if (!older->claim_present)
            return older->authority == DCENT_RECEIPT_AUTHORITY_OWNER;
        return older->authority == DCENT_RECEIPT_AUTHORITY_RECONCILER &&
               older->claim_status_revision == 3U;
    }
    return !older->claim_present &&
           older->authority == DCENT_RECEIPT_AUTHORITY_OWNER;
}

static enum dcent_receipt_storage_result classify_delta(
    const struct dcent_receipt_storage_head *older,
    const struct dcent_receipt_storage_head *newer,
    struct dcent_receipt_storage_delta *out)
{
    struct dcent_receipt_storage_delta parsed;
    size_t index;
    size_t changed_index = 0U;
    size_t changed_count = 0U;
    size_t added_index = 0U;

    if (older == NULL || newer == NULL || out == NULL ||
        !older->initialized || !newer->initialized)
        return DCENT_RECEIPT_STORAGE_INVALID_ARGUMENT;
    if (!head_links(older, newer))
        return DCENT_RECEIPT_STORAGE_SEMANTIC;
    memset(&parsed, 0, sizeof(parsed));

    if (newer->resource_count == older->resource_count + 1U) {
        if (older->claim_present || newer->claim_present ||
            older->authority != DCENT_RECEIPT_AUTHORITY_OWNER ||
            !authority_equal(older, newer) || !claim_equal(older, newer) ||
            !phase_equal(older, newer) ||
            older->transaction_phase != DCENT_RECEIPT_LOCK_ACTIVE ||
            !one_resource_added(older, newer, &added_index) ||
            newer->resources[added_index].status_revision != 1U)
            return DCENT_RECEIPT_STORAGE_SEMANTIC;
        parsed.kind = DCENT_RECEIPT_STORAGE_DELTA_RESOURCE_ADD;
        parsed.resource_kind = newer->resources[added_index].kind;
        parsed.object_id = newer->resources[added_index].resource_id;
        parsed.target_revision = 1U;
    } else if (newer->resource_count == older->resource_count) {
        for (index = 0U; index < older->resource_count; index++) {
            if (!resource_head_equal(&older->resources[index],
                                     &newer->resources[index])) {
                changed_index = index;
                changed_count++;
            }
        }
        if (changed_count == 1U && claim_equal(older, newer) &&
            authority_equal(older, newer) && phase_equal(older, newer) &&
            resource_mutation_phase_valid(older->transaction_phase) &&
            resource_advance_authority_valid(older)) {
            const struct dcent_receipt_storage_resource_head *old_resource =
                &older->resources[changed_index];
            const struct dcent_receipt_storage_resource_head *new_resource =
                &newer->resources[changed_index];

            if (!resource_identity_equal(old_resource, new_resource) ||
                !bytes_equal(old_resource->intent_sha256,
                             new_resource->intent_sha256,
                             DCENT_RECEIPT_SHA256_BYTES) ||
                old_resource->status_revision + 1U !=
                    new_resource->status_revision ||
                bytes_equal(old_resource->status_sha256,
                            new_resource->status_sha256,
                            DCENT_RECEIPT_SHA256_BYTES))
                return DCENT_RECEIPT_STORAGE_SEMANTIC;
            parsed.kind = DCENT_RECEIPT_STORAGE_DELTA_RESOURCE_ADVANCE;
            parsed.resource_kind = new_resource->kind;
            parsed.object_id = new_resource->resource_id;
            parsed.previous_revision = old_resource->status_revision;
            parsed.target_revision = new_resource->status_revision;
        } else if (changed_count == 0U && phase_equal(older, newer) &&
                   !older->claim_present &&
                   newer->claim_present &&
                   older->transaction_phase == DCENT_RECEIPT_LOCK_ACTIVE &&
                   older->authority == DCENT_RECEIPT_AUTHORITY_OWNER &&
                   newer->authority == DCENT_RECEIPT_AUTHORITY_RECONCILER &&
                   newer->claim_status_revision == 1U &&
                   id_compare(&newer->authority_id, &newer->claim_id) == 0) {
            parsed.kind = DCENT_RECEIPT_STORAGE_DELTA_CLAIM_ADD;
            parsed.object_id = newer->claim_id;
            parsed.target_revision = 1U;
        } else if (changed_count == 0U && phase_equal(older, newer) &&
                   older->claim_present &&
                   newer->claim_present && authority_equal(older, newer) &&
                   claim_mutation_phase_valid(older->transaction_phase) &&
                   id_compare(&older->claim_id, &newer->claim_id) == 0 &&
                   bytes_equal(older->claim_intent_sha256,
                               newer->claim_intent_sha256,
                               DCENT_RECEIPT_SHA256_BYTES) &&
                   older->claim_status_revision + 1U ==
                       newer->claim_status_revision &&
                   !bytes_equal(older->claim_status_sha256,
                                newer->claim_status_sha256,
                                DCENT_RECEIPT_SHA256_BYTES)) {
            parsed.kind = DCENT_RECEIPT_STORAGE_DELTA_CLAIM_ADVANCE;
            parsed.object_id = newer->claim_id;
            parsed.previous_revision = older->claim_status_revision;
            parsed.target_revision = newer->claim_status_revision;
        } else if (changed_count == 0U && claim_equal(older, newer) &&
                   authority_equal(older, newer) &&
                   older->transaction_phase_revision + 1U ==
                       newer->transaction_phase_revision &&
                   newer->transaction_phase_status_present &&
                   !bytes_equal(older->transaction_phase_status_sha256,
                                newer->transaction_phase_status_sha256,
                                DCENT_RECEIPT_SHA256_BYTES) &&
                   phase_advance_authority_valid(older, newer) &&
                   phase_transition_valid(older->transaction_phase,
                                          newer->transaction_phase)) {
            parsed.kind = DCENT_RECEIPT_STORAGE_DELTA_PHASE_ADVANCE;
            parsed.previous_revision = older->transaction_phase_revision;
            parsed.target_revision = newer->transaction_phase_revision;
            parsed.previous_phase = older->transaction_phase;
            parsed.target_phase = newer->transaction_phase;
        } else {
            return DCENT_RECEIPT_STORAGE_SEMANTIC;
        }
    } else {
        return DCENT_RECEIPT_STORAGE_SEMANTIC;
    }

    if (!all_resources_equal(older, newer) &&
        parsed.kind != DCENT_RECEIPT_STORAGE_DELTA_RESOURCE_ADD &&
        parsed.kind != DCENT_RECEIPT_STORAGE_DELTA_RESOURCE_ADVANCE)
        return DCENT_RECEIPT_STORAGE_SEMANTIC;
    parsed.initialized = true;
    *out = parsed;
    return DCENT_RECEIPT_STORAGE_OK;
}

enum dcent_receipt_storage_result
dcent_receipt_storage_validate_manifest_pair_abi2(
    const struct dcent_receipt_storage_seal *seal,
    const struct dcent_receipt_storage_head *bank0,
    const struct dcent_receipt_storage_head *bank1,
    struct dcent_receipt_storage_manifest_pair *out)
{
    struct dcent_receipt_storage_manifest_pair parsed;
    const struct dcent_receipt_storage_head *older;
    const struct dcent_receipt_storage_head *newer;
    unsigned int older_bank;
    unsigned int newer_bank;
    enum dcent_receipt_storage_result result;

    if (seal == NULL || bank0 == NULL || bank1 == NULL || out == NULL)
        return DCENT_RECEIPT_STORAGE_INVALID_ARGUMENT;
    if (!head_projection_valid(seal, bank0) ||
        !head_projection_valid(seal, bank1))
        return DCENT_RECEIPT_STORAGE_SEMANTIC;

    memset(&parsed, 0, sizeof(parsed));
    if (bank0->generation == bank1->generation) {
        if (bank0->generation != 0U ||
            !bytes_equal(bank0->record_sha256, bank1->record_sha256,
                         DCENT_RECEIPT_SHA256_BYTES))
            return DCENT_RECEIPT_STORAGE_SEMANTIC;
        parsed.genesis = true;
        parsed.previous_bank = 1U;
        parsed.current_bank = 0U;
        parsed.current_generation = 0U;
        memcpy(parsed.current_head_sha256, bank0->record_sha256,
               DCENT_RECEIPT_SHA256_BYTES);
        parsed.initialized = true;
        *out = parsed;
        return DCENT_RECEIPT_STORAGE_OK;
    }

    if (bank0->generation < bank1->generation) {
        older = bank0;
        newer = bank1;
        older_bank = 0U;
        newer_bank = 1U;
    } else {
        older = bank1;
        newer = bank0;
        older_bank = 1U;
        newer_bank = 0U;
    }
    if (newer_bank != newer->generation % 2U ||
        older_bank != older->generation % 2U || !head_links(older, newer))
        return DCENT_RECEIPT_STORAGE_SEMANTIC;
    result = classify_delta(older, newer, &parsed.delta);
    if (result != DCENT_RECEIPT_STORAGE_OK)
        return result;

    parsed.genesis = false;
    parsed.previous_bank = older_bank;
    parsed.current_bank = newer_bank;
    parsed.current_generation = newer->generation;
    memcpy(parsed.current_head_sha256, newer->record_sha256,
           DCENT_RECEIPT_SHA256_BYTES);
    parsed.initialized = true;
    *out = parsed;
    return DCENT_RECEIPT_STORAGE_OK;
}

const char *dcent_receipt_storage_result_name(
    enum dcent_receipt_storage_result result)
{
    switch (result) {
    case DCENT_RECEIPT_STORAGE_OK:
        return "ok";
    case DCENT_RECEIPT_STORAGE_INVALID_ARGUMENT:
        return "invalid-argument";
    case DCENT_RECEIPT_STORAGE_MALFORMED:
        return "malformed";
    case DCENT_RECEIPT_STORAGE_LIMIT:
        return "limit";
    case DCENT_RECEIPT_STORAGE_SEMANTIC:
        return "semantic";
    }
    return "unknown";
}
