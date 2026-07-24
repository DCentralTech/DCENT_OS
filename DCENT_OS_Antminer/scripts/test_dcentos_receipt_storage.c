/* SPDX-License-Identifier: GPL-3.0-or-later */
#include "receipt_storage.h"

#include <stdarg.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define HEX0 "0000000000000000000000000000000000000000000000000000000000000000"
#define HEX1 "1111111111111111111111111111111111111111111111111111111111111111"
#define HEX2 "2222222222222222222222222222222222222222222222222222222222222222"
#define HEX3 "3333333333333333333333333333333333333333333333333333333333333333"
#define HEX4 "4444444444444444444444444444444444444444444444444444444444444444"
#define HEX5 "5555555555555555555555555555555555555555555555555555555555555555"
#define HEXA "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

static unsigned int assertions;

static void require(bool condition, const char *message)
{
    assertions++;
    if (!condition) {
        fprintf(stderr, "FAIL: %s\n", message);
        exit(1);
    }
}

static void appendf(char *buffer, size_t capacity, size_t *used,
                    const char *format, ...)
{
    va_list arguments;
    int written;

    require(*used < capacity, "test builder retains output capacity");
    va_start(arguments, format);
    written = vsnprintf(buffer + *used, capacity - *used, format, arguments);
    va_end(arguments);
    require(written >= 0 && (size_t)written < capacity - *used,
            "test builder output fits");
    *used += (size_t)written;
}

struct test_row {
    const char *kind;
    const char *id;
    const char *intent;
    unsigned int revision;
    const char *status;
};

static size_t build_seal(char *buffer, size_t capacity, const char *tx,
                         const char *boot, const char *guard_devino,
                         const char *lock_devino, const char *owner_devino,
                         const char *owner_digest, const char *mount_id,
                         const char *ledger_devino, const char *binding,
                         const char *lease_devino, const char *initial_phase,
                         const char *owner)
{
    size_t used = 0U;

    appendf(buffer, capacity, &used,
            "schema=dcentos-sysupgrade-ledger-seal-abi2\n"
            "transaction_id=%s\n"
            "boot_id=%s\n"
            "acquisition_guard_device_inode=%s\n"
            "transaction_lock_device_inode=%s\n"
            "transaction_lock_owner_device_inode=%s\n"
            "transaction_lock_owner_sha256=%s\n"
            "storage_mount_id=%s\n"
            "ledger_device_inode=%s\n"
            "binding_sha256=%s\n"
            "mutation_lease_device_inode=%s\n"
            "initial_transaction_phase=%s\n"
            "owner=%s\n",
            tx, boot, guard_devino, lock_devino, owner_devino, owner_digest,
            mount_id, ledger_devino, binding, lease_devino, initial_phase,
            owner);
    return used;
}

static size_t build_head_phase(
    char *buffer, size_t capacity, const char *seal,
    unsigned int generation, bool previous_present,
    unsigned int previous_generation, const char *previous_digest,
    const char *authority, const char *authority_id, const char *phase,
    unsigned int phase_revision, const char *phase_digest, bool claim_present,
                         const char *claim_id, const char *claim_intent,
                         unsigned int claim_revision,
                         const char *claim_status, const struct test_row *rows,
                         size_t row_count)
{
    size_t used = 0U;
    size_t index;

    appendf(buffer, capacity, &used,
            "schema=dcentos-sysupgrade-ledger-head-abi2\n"
            "seal_sha256=%s\n"
            "generation=%u\n",
            seal, generation);
    if (previous_present) {
        appendf(buffer, capacity, &used,
                "previous_generation=%u\n"
                "previous_head_sha256=%s\n",
                previous_generation, previous_digest);
    } else {
        appendf(buffer, capacity, &used,
                "previous_generation=-\nprevious_head_sha256=-\n");
    }
    appendf(buffer, capacity, &used,
            "authority_kind=%s\n"
            "authority_id=%s\n"
            "transaction_phase=%s\n"
            "transaction_phase_revision=%u\n"
            "transaction_phase_status_sha256=%s\n"
            "claim_present=%u\n"
            "claim_id=%s\n"
            "claim_intent_sha256=%s\n"
            "claim_status_revision=%u\n"
            "claim_status_sha256=%s\n"
            "resource_count=%zu\n",
            authority, authority_id, phase, phase_revision, phase_digest,
            claim_present ? 1U : 0U, claim_id, claim_intent, claim_revision,
            claim_status, row_count);
    for (index = 0U; index < row_count; index++) {
        appendf(buffer, capacity, &used,
                "resource.%02zu=%s:%s:%s:%u:%s\n", index,
                rows[index].kind, rows[index].id, rows[index].intent,
                rows[index].revision, rows[index].status);
    }
    return used;
}

static size_t build_head(char *buffer, size_t capacity, const char *seal,
                         unsigned int generation, bool previous_present,
                         unsigned int previous_generation,
                         const char *previous_digest, const char *authority,
                         const char *authority_id, bool claim_present,
                         const char *claim_id, const char *claim_intent,
                         unsigned int claim_revision,
                         const char *claim_status, const struct test_row *rows,
                         size_t row_count)
{
    return build_head_phase(
        buffer, capacity, seal, generation, previous_present,
        previous_generation, previous_digest, authority, authority_id,
        "active", 0U, "-", claim_present, claim_id, claim_intent,
        claim_revision, claim_status, rows, row_count);
}

static void digest_hex(
    char output[DCENT_RECEIPT_SHA256_HEX_BYTES + 1U],
    const unsigned char digest[DCENT_RECEIPT_SHA256_BYTES])
{
    dcent_receipt_sha256_hex(output, digest);
}

static void require_seal_failure(const void *data, size_t size,
                                 const char *message)
{
    struct dcent_receipt_storage_seal output;
    struct dcent_receipt_storage_seal before;

    memset(&output, 0xa5, sizeof(output));
    before = output;
    require(dcent_receipt_storage_parse_seal_abi2(data, size, &output) !=
                DCENT_RECEIPT_STORAGE_OK,
            message);
    require(memcmp(&output, &before, sizeof(output)) == 0,
            "failed seal parse is output-failure-atomic");
}

static void require_head_failure(const void *data, size_t size,
                                 const char *message)
{
    struct dcent_receipt_storage_head output;
    struct dcent_receipt_storage_head before;

    memset(&output, 0x5a, sizeof(output));
    before = output;
    require(dcent_receipt_storage_parse_head_abi2(data, size, &output) !=
                DCENT_RECEIPT_STORAGE_OK,
            message);
    require(memcmp(&output, &before, sizeof(output)) == 0,
            "failed head parse is output-failure-atomic");
}

static struct dcent_receipt_storage_head parse_head(const char *bytes,
                                                    size_t size,
                                                    const char *message)
{
    struct dcent_receipt_storage_head output;

    memset(&output, 0, sizeof(output));
    require(dcent_receipt_storage_parse_head_abi2(bytes, size, &output) ==
                DCENT_RECEIPT_STORAGE_OK,
            message);
    return output;
}

static void test_seal_parser(struct dcent_receipt_storage_seal *seal,
                             char seal_hex[65])
{
    static const char boot[] = "01234567-89ab-cdef-0123-456789abcdef";
    char bytes[5000];
    char mutated[5000];
    char long_id[65];
    size_t size;
    size_t index;

    size = build_seal(bytes, sizeof(bytes), "tx.main", boot, "5:9", "5:10",
                      "5:13", HEX0, "42", "5:11", HEXA, "5:12", "active",
                      "zynq-sysupgrade");
    memset(seal, 0, sizeof(*seal));
    require(dcent_receipt_storage_parse_seal_abi2(bytes, size, seal) ==
                DCENT_RECEIPT_STORAGE_OK,
            "canonical ABI2 seal parses");
    require(seal->initialized && seal->transaction_id.size == 7U &&
                seal->boot_id.size == 36U &&
                seal->acquisition_guard_device_inode.inode == 9U &&
                seal->transaction_lock_device_inode.device == 5U &&
                seal->transaction_lock_device_inode.inode == 10U &&
                seal->transaction_lock_owner_device_inode.inode == 13U &&
                seal->storage_mount_id == 42U &&
                seal->ledger_device_inode.inode == 11U &&
                seal->mutation_lease_device_inode.inode == 12U,
            "seal parser owns every identity field");
    require(seal->binding_sha256[0] == 0xaaU &&
                seal->binding_sha256[31] == 0xaaU,
            "seal parser decodes the binding digest");
    digest_hex(seal_hex, seal->record_sha256);

    for (index = 0U; index < size; index++)
        require_seal_failure(bytes, index, "every seal truncation is refused");
    require_seal_failure(NULL, size, "null seal bytes are refused");
    require(dcent_receipt_storage_parse_seal_abi2(bytes, size, NULL) ==
                DCENT_RECEIPT_STORAGE_INVALID_ARGUMENT,
            "null seal output is refused");

    size = build_seal(mutated, sizeof(mutated), "-bad", boot, "5:9", "5:10",
                      "5:13", HEX0, "42", "5:11", HEXA, "5:12", "active",
                      "zynq-sysupgrade");
    require_seal_failure(mutated, size, "noncanonical seal ID is refused");
    size = build_seal(mutated, sizeof(mutated), "tx", "01234567-89AB-cdef-0123-456789abcdef",
                      "5:9", "5:10", "5:13", HEX0, "42", "5:11", HEXA,
                      "5:12", "active", "zynq-sysupgrade");
    require_seal_failure(mutated, size, "uppercase seal UUID is refused");
    size = build_seal(mutated, sizeof(mutated), "tx", boot, "05:9", "5:10",
                      "5:13", HEX0, "42", "5:11", HEXA, "5:12", "active",
                      "zynq-sysupgrade");
    require_seal_failure(mutated, size, "noncanonical seal devino is refused");
    size = build_seal(mutated, sizeof(mutated), "tx", boot, "5:9", "5:10",
                      "5:13", HEX0, "42", "5:11",
                      "Aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                      "5:12", "active", "zynq-sysupgrade");
    require_seal_failure(mutated, size, "uppercase seal digest is refused");
    size = build_seal(mutated, sizeof(mutated), "tx", boot, "5:9", "5:10",
                      "5:13", HEX0, "42", "5:11", HEXA, "5:12", "active",
                      "other");
    require_seal_failure(mutated, size, "wrong seal owner is refused");
    size = build_seal(mutated, sizeof(mutated), "tx", boot, "5:9", "5:10",
                      "5:13", HEX0, "042", "5:11", HEXA, "5:12", "active",
                      "zynq-sysupgrade");
    require_seal_failure(mutated, size,
                         "noncanonical storage mount ID is refused");
    size = build_seal(mutated, sizeof(mutated), "tx", boot, "5:9", "5:10",
                      "5:13", HEX0, "0", "5:11", HEXA, "5:12", "active",
                      "zynq-sysupgrade");
    require_seal_failure(mutated, size,
                         "zero storage mount ID is refused");
    size = build_seal(mutated, sizeof(mutated), "tx", boot, "5:9", "5:10",
                      "5:13", HEX0, "42", "5:11", HEXA, "5:12",
                      "cleanup-required", "zynq-sysupgrade");
    require_seal_failure(mutated, size,
                         "seal initial transaction phase is exactly active");
    memset(long_id, 't', 64U);
    long_id[64] = '\0';
    size = build_seal(mutated, sizeof(mutated), long_id, boot, "5:9", "5:10",
                      "5:13", HEX0, "42", "5:11", HEXA, "5:12", "active",
                      "zynq-sysupgrade");
    {
        struct dcent_receipt_storage_seal maximum_id_seal;

        require(dcent_receipt_storage_parse_seal_abi2(
                    mutated, size, &maximum_id_seal) ==
                    DCENT_RECEIPT_STORAGE_OK &&
                    maximum_id_seal.transaction_id.size == 64U,
                "seal owns an exact 64-byte transaction ID");
    }

    {
        char oversized[DCENT_RECEIPT_STORAGE_MAX_SEAL + 2U];

        memset(oversized, 'x', sizeof(oversized));
        oversized[sizeof(oversized) - 1U] = '\n';
        require_seal_failure(oversized, sizeof(oversized),
                             "oversized seal is refused");
    }
}

static void test_head_parsers_and_limits(
    const struct dcent_receipt_storage_seal *seal, const char *seal_hex,
    struct dcent_receipt_storage_head *genesis, char genesis_hex[65])
{
    char bytes[9000];
    char second[9000];
    struct test_row row = {"mount", "mnt0", HEX1, 1U, HEX2};
    struct test_row unsorted[2] = {
        {"node", "z", HEX1, 1U, HEX2},
        {"node", "a", HEX2, 1U, HEX3},
    };
    struct test_row maximum[32];
    char ids[32][65];
    char long_claim[65];
    size_t size;
    size_t index;

    size = build_head(bytes, sizeof(bytes), seal_hex, 0U, false, 0U, "-",
                      "owner", "tx.main", false, "-", "-", 0U, "-", NULL,
                      0U);
    *genesis = parse_head(bytes, size, "canonical genesis head parses");
    require(genesis->initialized && genesis->generation == 0U &&
                !genesis->previous_present && !genesis->claim_present &&
                genesis->resource_count == 0U &&
                genesis->authority == DCENT_RECEIPT_AUTHORITY_OWNER,
            "genesis head owns exact empty state");
    digest_hex(genesis_hex, genesis->record_sha256);

    for (index = 0U; index < size; index++)
        require_head_failure(bytes, index, "every head truncation is refused");
    require_head_failure(NULL, size, "null head bytes are refused");
    require(dcent_receipt_storage_parse_head_abi2(bytes, size, NULL) ==
                DCENT_RECEIPT_STORAGE_INVALID_ARGUMENT,
            "null head output is refused");

    size = build_head(second, sizeof(second), seal_hex, 1U, true, 0U,
                      genesis_hex, "owner", "tx.main", false, "-", "-", 0U,
                      "-", &row, 1U);
    {
        struct dcent_receipt_storage_head parsed =
            parse_head(second, size, "one-resource head parses");

        require(parsed.generation == 1U && parsed.resource_count == 1U &&
                    parsed.resources[0].kind == DCENT_RECEIPT_KIND_MOUNT &&
                    parsed.resources[0].status_revision == 1U,
                "resource row is decoded into owned fixed storage");
    }

    size = build_head(second, sizeof(second), seal_hex, 2U, true, 1U, HEX0,
                      "owner", "tx.main", false, "-", "-", 0U, "-", &row,
                      1U);
    require_head_failure(second, size,
                         "generation must equal committed revision sum");
    size = build_head(second, sizeof(second), seal_hex, 0U, true, 0U, HEX0,
                      "owner", "tx.main", false, "-", "-", 0U, "-", NULL,
                      0U);
    require_head_failure(second, size,
                         "genesis cannot authenticate a previous head");
    size = build_head(second, sizeof(second), seal_hex, 1U, true, 1U, HEX0,
                      "owner", "tx.main", false, "-", "-", 0U, "-", &row,
                      1U);
    require_head_failure(second, size,
                         "previous generation must immediately precede head");
    size = build_head(second, sizeof(second), seal_hex, 1U, true, 0U, HEX0,
                      "reconciler", "tx.main", false, "-", "-", 0U, "-",
                      &row, 1U);
    require_head_failure(second, size,
                         "eventless head cannot claim reconciler authority");
    size = build_head(second, sizeof(second), seal_hex, 1U, true, 0U, HEX0,
                      "owner", "tx.main", true, "claim1", HEX1, 1U, HEX2,
                      NULL, 0U);
    require_head_failure(second, size,
                         "claim must atomically transfer authority");
    size = build_head(second, sizeof(second), seal_hex, 1U, true, 0U, HEX0,
                      "reconciler", "wrong", true, "claim1", HEX1, 1U, HEX2,
                      NULL, 0U);
    require_head_failure(second, size,
                         "reconciler authority ID must equal claim ID");
    size = build_head(second, sizeof(second), seal_hex, 0U, false, 0U, "-",
                      "owner", "tx.main", false, "claim1", "-", 0U, "-",
                      NULL, 0U);
    require_head_failure(second, size,
                         "absent claim requires exact dash sentinels");
    size = build_head(second, sizeof(second), seal_hex, 2U, true, 1U, HEX0,
                      "owner", "tx.main", false, "-", "-", 0U, "-",
                      unsorted, 2U);
    require_head_failure(second, size,
                         "resource rows must be strictly sorted");
    unsorted[1] = unsorted[0];
    size = build_head(second, sizeof(second), seal_hex, 2U, true, 1U, HEX0,
                      "owner", "tx.main", false, "-", "-", 0U, "-",
                      unsorted, 2U);
    require_head_failure(second, size,
                         "duplicate resource identities are refused");

    size = build_head_phase(
        second, sizeof(second), seal_hex, 1U, true, 0U, genesis_hex,
        "owner", "tx.main", "cleanup-required", 1U, HEX1, false, "-", "-",
        0U, "-", NULL, 0U);
    {
        struct dcent_receipt_storage_head parsed =
            parse_head(second, size, "phase revision projection parses");

        require(parsed.transaction_phase ==
                    DCENT_RECEIPT_LOCK_CLEANUP_REQUIRED &&
                    parsed.transaction_phase_revision == 1U &&
                    parsed.transaction_phase_status_present,
                "head owns the complete transaction-phase projection");
    }
    size = build_head_phase(second, sizeof(second), seal_hex, 0U, false, 0U,
                            "-", "owner", "tx.main", "env-commit-armed", 0U,
                            "-", false, "-", "-", 0U, "-", NULL, 0U);
    require_head_failure(second, size,
                         "implicit phase revision zero is exactly active");
    size = build_head_phase(second, sizeof(second), seal_hex, 1U, true, 0U,
                            genesis_hex, "owner", "tx.main", "active", 1U,
                            "-", false, "-", "-", 0U, "-", NULL, 0U);
    require_head_failure(second, size,
                         "nonzero phase revision requires a status digest");
    size = build_head_phase(second, sizeof(second), seal_hex, 17U, true, 16U,
                            HEX0, "owner", "tx.main", "active", 17U, HEX1,
                            false, "-", "-", 0U, "-", NULL, 0U);
    require_head_failure(second, size,
                         "seventeenth transaction-phase revision is refused");

    memset(long_claim, 'c', 64U);
    long_claim[64] = '\0';
    for (index = 0U; index < 32U; index++) {
        memset(ids[index], 'a', 64U);
        ids[index][0] = 'r';
        ids[index][62] = (char)('0' + index / 10U);
        ids[index][63] = (char)('0' + index % 10U);
        ids[index][64] = '\0';
        maximum[index].kind = "attachment";
        maximum[index].id = ids[index];
        maximum[index].intent = HEX1;
        maximum[index].revision = 4U;
        maximum[index].status = HEX2;
    }
    size = build_head_phase(
        second, sizeof(second), seal_hex, 148U, true, 147U, HEX0,
        "reconciler", long_claim, "env-commit-armed", 16U, HEX5, true,
        long_claim, HEX3, 4U, HEX4, maximum, 32U);
    require(size == 7853U && size <= DCENT_RECEIPT_STORAGE_MAX_HEAD,
            "true 7853-byte maximum canonical head is locked");
    {
        struct dcent_receipt_storage_head parsed =
            parse_head(second, size, "generation-148 maximum head parses");

        require(parsed.resource_count == 32U && parsed.generation == 148U &&
                    parsed.resources[31].status_revision == 4U &&
                    parsed.claim_status_revision == 4U &&
                    parsed.transaction_phase_revision == 16U,
                "maximum head retains all bounded rows and revisions");
    }

    size = build_head_phase(
        second, sizeof(second), seal_hex, 149U, true, 148U, HEX0,
        "reconciler", long_claim, "env-commit-armed", 16U, HEX5, true,
        long_claim, HEX3, 4U, HEX4, maximum, 32U);
    require_head_failure(second, size, "generation above 148 is refused");
    {
        char oversized[DCENT_RECEIPT_STORAGE_MAX_HEAD + 2U];

        memset(oversized, 'x', sizeof(oversized));
        oversized[sizeof(oversized) - 1U] = '\n';
        require_head_failure(oversized, sizeof(oversized),
                             "oversized head is refused before parsing");
    }
    require(seal->initialized, "seal remains owned after head parser tests");
}

static struct dcent_receipt_storage_head make_head(
    const char *seal_hex, unsigned int generation,
    const struct dcent_receipt_storage_head *previous, const char *authority,
    const char *authority_id, bool claim_present, const char *claim_id,
    const char *claim_intent, unsigned int claim_revision,
    const char *claim_status, const struct test_row *rows, size_t row_count,
    char output_hex[65])
{
    char bytes[9000];
    char previous_hex[65];
    size_t size;
    struct dcent_receipt_storage_head parsed;

    if (previous != NULL)
        digest_hex(previous_hex, previous->record_sha256);
    size = build_head(bytes, sizeof(bytes), seal_hex, generation,
                      previous != NULL,
                      previous != NULL ? previous->generation : 0U,
                      previous != NULL ? previous_hex : "-", authority,
                      authority_id, claim_present, claim_id, claim_intent,
                      claim_revision, claim_status, rows, row_count);
    parsed = parse_head(bytes, size, "generated linked head parses");
    digest_hex(output_hex, parsed.record_sha256);
    return parsed;
}

static struct dcent_receipt_storage_head make_phase_head(
    const char *seal_hex, unsigned int generation,
    const struct dcent_receipt_storage_head *previous, const char *authority,
    const char *authority_id, const char *phase, unsigned int phase_revision,
    const char *phase_digest, bool claim_present, const char *claim_id,
    const char *claim_intent, unsigned int claim_revision,
    const char *claim_status, const struct test_row *rows, size_t row_count,
    char output_hex[65])
{
    char bytes[9000];
    char previous_hex[65];
    size_t size;
    struct dcent_receipt_storage_head parsed;

    digest_hex(previous_hex, previous->record_sha256);
    size = build_head_phase(
        bytes, sizeof(bytes), seal_hex, generation, true,
        previous->generation, previous_hex, authority, authority_id, phase,
        phase_revision, phase_digest, claim_present, claim_id, claim_intent,
        claim_revision, claim_status, rows, row_count);
    parsed = parse_head(bytes, size, "generated phase-linked head parses");
    digest_hex(output_hex, parsed.record_sha256);
    return parsed;
}

static void require_pair_failure(
    const struct dcent_receipt_storage_seal *seal,
    const struct dcent_receipt_storage_head *bank0,
    const struct dcent_receipt_storage_head *bank1, const char *message)
{
    struct dcent_receipt_storage_manifest_pair output;
    struct dcent_receipt_storage_manifest_pair before;

    memset(&output, 0xa5, sizeof(output));
    before = output;
    require(dcent_receipt_storage_validate_manifest_pair_abi2(
                seal, bank0, bank1, &output) !=
                DCENT_RECEIPT_STORAGE_OK,
            message);
    require(memcmp(&output, &before, sizeof(output)) == 0,
            "failed pair validation is output-failure-atomic");
}

static void require_delta(
    const struct dcent_receipt_storage_seal *seal,
    const struct dcent_receipt_storage_head *older,
    const struct dcent_receipt_storage_head *newer,
    enum dcent_receipt_storage_delta_kind expected, const char *message)
{
    struct dcent_receipt_storage_manifest_pair pair;

    memset(&pair, 0, sizeof(pair));
    require(dcent_receipt_storage_validate_manifest_pair_abi2(
                seal, newer->generation % 2U == 0U ? newer : older,
                newer->generation % 2U == 0U ? older : newer, &pair) ==
                DCENT_RECEIPT_STORAGE_OK,
            message);
    require(pair.initialized && pair.delta.initialized &&
                pair.delta.kind == expected,
            "delta classifier returns the exact mutation kind");
}

static void require_delta_failure(
    const struct dcent_receipt_storage_seal *seal,
    const struct dcent_receipt_storage_head *older,
    const struct dcent_receipt_storage_head *newer, const char *message)
{
    struct dcent_receipt_storage_manifest_pair output;
    struct dcent_receipt_storage_manifest_pair before;

    memset(&output, 0x5a, sizeof(output));
    before = output;
    require(dcent_receipt_storage_validate_manifest_pair_abi2(
                seal, newer->generation % 2U == 0U ? newer : older,
                newer->generation % 2U == 0U ? older : newer, &output) !=
                DCENT_RECEIPT_STORAGE_OK,
            message);
    require(memcmp(&output, &before, sizeof(output)) == 0,
            "failed delta classification is output-failure-atomic");
}

static void test_pair_and_delta_validation(
    const struct dcent_receipt_storage_seal *seal, const char *seal_hex,
    const struct dcent_receipt_storage_head *genesis)
{
    struct test_row rows[2] = {
        {"mount", "mnt0", HEX1, 1U, HEX2},
        {"workspace", "work0", HEX3, 1U, HEX4},
    };
    char head_hex[7][65];
    char phase_hex[6][65];
    struct dcent_receipt_storage_head heads[7];
    struct dcent_receipt_storage_head phase_heads[6];
    struct dcent_receipt_storage_manifest_pair pair;
    struct dcent_receipt_storage_head altered;
    struct dcent_receipt_storage_seal altered_seal;

    memset(&pair, 0, sizeof(pair));
    require(dcent_receipt_storage_validate_manifest_pair_abi2(
                seal, genesis, genesis, &pair) ==
                DCENT_RECEIPT_STORAGE_OK,
            "byte-identical generation-zero banks validate");
    require(pair.initialized && pair.genesis && pair.current_bank == 0U &&
                pair.previous_bank == 1U && pair.current_generation == 0U,
            "genesis pair selection is owned and deterministic");

    heads[0] = make_head(seal_hex, 1U, genesis, "owner", "tx.main", false,
                         "-", "-", 0U, "-", rows, 1U, head_hex[0]);
    memset(&pair, 0, sizeof(pair));
    require(dcent_receipt_storage_validate_manifest_pair_abi2(
                seal, genesis, &heads[0], &pair) ==
                DCENT_RECEIPT_STORAGE_OK &&
                !pair.genesis && pair.previous_bank == 0U &&
                pair.current_bank == 1U,
            "linked zero/one banks select the newer bank");
    require_delta(seal, genesis, &heads[0],
                  DCENT_RECEIPT_STORAGE_DELTA_RESOURCE_ADD,
                  "first resource addition is classified");

    rows[0].revision = 2U;
    rows[0].status = HEX3;
    heads[1] = make_head(seal_hex, 2U, &heads[0], "owner", "tx.main", false,
                         "-", "-", 0U, "-", rows, 1U, head_hex[1]);
    require_delta(seal, &heads[0], &heads[1],
                  DCENT_RECEIPT_STORAGE_DELTA_RESOURCE_ADVANCE,
                  "owner resource advance is classified");

    heads[2] = make_head(seal_hex, 3U, &heads[1], "reconciler", "claim1",
                         true, "claim1", HEX4, 1U, HEX5, rows, 1U,
                         head_hex[2]);
    require_delta(seal, &heads[1], &heads[2],
                  DCENT_RECEIPT_STORAGE_DELTA_CLAIM_ADD,
                  "claim addition and authority transfer are classified");
    heads[3] = make_head(seal_hex, 4U, &heads[2], "reconciler", "claim1",
                         true, "claim1", HEX4, 2U, HEX0, rows, 1U,
                         head_hex[3]);
    require_delta(seal, &heads[2], &heads[3],
                  DCENT_RECEIPT_STORAGE_DELTA_CLAIM_ADVANCE,
                  "claim advance is classified");
    heads[4] = make_head(seal_hex, 5U, &heads[3], "reconciler", "claim1",
                         true, "claim1", HEX4, 3U, HEX1, rows, 1U,
                         head_hex[4]);
    rows[0].revision = 3U;
    rows[0].status = HEX4;
    altered = make_head(seal_hex, 5U, &heads[3], "reconciler", "claim1",
                        true, "claim1", HEX4, 2U, HEX0, rows, 1U,
                        phase_hex[5]);
    require_delta_failure(
        seal, &heads[3], &altered,
        "reconciler resource advance before claim revision 3 is refused");
    heads[5] = make_head(seal_hex, 6U, &heads[4], "reconciler", "claim1",
                         true, "claim1", HEX4, 3U, HEX1, rows, 1U,
                         head_hex[5]);
    require_delta(seal, &heads[4], &heads[5],
                  DCENT_RECEIPT_STORAGE_DELTA_RESOURCE_ADVANCE,
                  "reconciler resource advance is structurally classified");

    rows[1].revision = 1U;
    heads[6] = make_head(seal_hex, 7U, &heads[5], "reconciler", "claim1",
                         true, "claim1", HEX4, 3U, HEX1, rows, 2U,
                         head_hex[6]);
    require_delta_failure(seal, &heads[5], &heads[6],
                          "resource addition after claim is refused");

    phase_heads[0] = make_phase_head(
        seal_hex, 1U, genesis, "owner", "tx.main", "env-commit-armed", 1U,
        HEX2, false, "-", "-", 0U, "-", NULL, 0U, phase_hex[0]);
    require_delta(seal, genesis, &phase_heads[0],
                  DCENT_RECEIPT_STORAGE_DELTA_PHASE_ADVANCE,
                  "owner active to env-commit-armed phase advance is classified");
    phase_heads[1] = make_phase_head(
        seal_hex, 2U, &phase_heads[0], "owner", "tx.main", "active", 2U,
        HEX3, false, "-", "-", 0U, "-", NULL, 0U, phase_hex[1]);
    require_delta(seal, &phase_heads[0], &phase_heads[1],
                  DCENT_RECEIPT_STORAGE_DELTA_PHASE_ADVANCE,
                  "authenticated env-commit disarm is classified");
    phase_heads[2] = make_phase_head(
        seal_hex, 3U, &phase_heads[1], "owner", "tx.main",
        "env-commit-armed", 3U, HEX4, false, "-", "-", 0U, "-", NULL,
        0U, phase_hex[2]);
    require_delta(seal, &phase_heads[1], &phase_heads[2],
                  DCENT_RECEIPT_STORAGE_DELTA_PHASE_ADVANCE,
                  "second env-commit arm is classified");
    phase_heads[3] = make_phase_head(
        seal_hex, 4U, &phase_heads[2], "owner", "tx.main", "env-committed",
        4U, HEX5, false, "-", "-", 0U, "-", NULL, 0U, phase_hex[3]);
    require_delta(seal, &phase_heads[2], &phase_heads[3],
                  DCENT_RECEIPT_STORAGE_DELTA_PHASE_ADVANCE,
                  "env-commit-armed to committed is classified");
    phase_heads[4] = make_phase_head(
        seal_hex, 7U, &heads[5], "reconciler", "claim1",
        "cleanup-required", 1U, HEX2, true, "claim1", HEX4, 3U, HEX1, rows,
        1U, phase_hex[4]);
    require_delta(seal, &heads[5], &phase_heads[4],
                  DCENT_RECEIPT_STORAGE_DELTA_PHASE_ADVANCE,
                  "reconciling claimant may publish cleanup-required");

    phase_heads[5] = make_phase_head(
        seal_hex, 7U, &heads[5], "reconciler", "claim1",
        "env-commit-armed", 1U, HEX2, true, "claim1", HEX4, 3U, HEX1, rows,
        1U, phase_hex[5]);
    require_delta_failure(
        seal, &heads[5], &phase_heads[5],
        "reconciler authority cannot arm the boot environment");

    phase_heads[5] = make_phase_head(
        seal_hex, 2U, &phase_heads[0], "reconciler", "claim1",
        "env-commit-armed", 1U, HEX2, true, "claim1", HEX4, 1U, HEX1, NULL,
        0U, phase_hex[5]);
    require_delta_failure(seal, &phase_heads[0], &phase_heads[5],
                          "claim transfer while environment commit is armed is refused");

    phase_heads[5] = make_phase_head(
        seal_hex, 7U, &heads[5], "reconciler", "claim1", "active", 1U, HEX2,
        true, "claim1", HEX4, 3U, HEX1, rows, 1U, phase_hex[5]);
    require_delta_failure(seal, &heads[5], &phase_heads[5],
                          "active to active phase record is refused");
    phase_heads[5] = make_phase_head(
        seal_hex, 7U, &heads[5], "reconciler", "claim1", "env-committed", 1U,
        HEX2, true, "claim1", HEX4, 3U, HEX1, rows, 1U, phase_hex[5]);
    require_delta_failure(seal, &heads[5], &phase_heads[5],
                          "active to env-committed skip is refused");
    phase_heads[5] = make_phase_head(
        seal_hex, 2U, &phase_heads[0], "owner", "tx.main",
        "cleanup-required", 2U, HEX3, false, "-", "-", 0U, "-", NULL, 0U,
        phase_hex[5]);
    require_delta_failure(seal, &phase_heads[0], &phase_heads[5],
                          "env-commit-armed to cleanup-required is refused");
    phase_heads[5] = make_phase_head(
        seal_hex, 8U, &phase_heads[4], "reconciler", "claim1", "active", 2U,
        HEX3, true, "claim1", HEX4, 3U, HEX1, rows, 1U, phase_hex[5]);
    require_delta_failure(seal, &phase_heads[4], &phase_heads[5],
                          "cleanup-required is terminal");
    phase_heads[5] = make_phase_head(
        seal_hex, 5U, &phase_heads[3], "owner", "tx.main", "active", 5U,
        HEX0, false, "-", "-", 0U, "-", NULL, 0U, phase_hex[5]);
    require_delta_failure(seal, &phase_heads[3], &phase_heads[5],
                          "env-committed is terminal");
    {
        static const char *const phase_names[4] = {
            "active", "cleanup-required", "env-commit-armed",
            "env-committed",
        };
        static const char *const phase_digests[4] = {HEX0, HEX1, HEX2, HEX3};
        static const bool legal[4][4] = {
            {false, true, true, false},
            {false, false, false, false},
            {true, false, false, true},
            {false, false, false, false},
        };
        const struct dcent_receipt_storage_head *bases[4] = {
            genesis, &phase_heads[4], &phase_heads[0], &phase_heads[3],
        };
        size_t from;
        size_t to;

        for (from = 0U; from < 4U; from++) {
            for (to = 0U; to < 4U; to++) {
                struct dcent_receipt_storage_head target;
                char target_hex[65];
                bool claimed = bases[from]->claim_present;

                target = make_phase_head(
                    seal_hex, bases[from]->generation + 1U, bases[from],
                    claimed ? "reconciler" : "owner",
                    claimed ? "claim1" : "tx.main", phase_names[to],
                    bases[from]->transaction_phase_revision + 1U,
                    phase_digests[to], claimed,
                    claimed ? "claim1" : "-", claimed ? HEX4 : "-",
                    claimed ? 3U : 0U, claimed ? HEX1 : "-",
                    claimed ? rows : NULL, claimed ? 1U : 0U, target_hex);
                if (legal[from][to])
                    require_delta(seal, bases[from], &target,
                                  DCENT_RECEIPT_STORAGE_DELTA_PHASE_ADVANCE,
                                  "legal bounded phase matrix edge validates");
                else
                    require_delta_failure(
                        seal, bases[from], &target,
                        "illegal bounded phase matrix edge is refused");
            }
        }
    }
    rows[0].revision = 1U;
    rows[0].status = HEX5;
    phase_heads[5] = make_phase_head(
        seal_hex, 2U, &phase_heads[0], "owner", "tx.main",
        "env-commit-armed", 1U, HEX2, false, "-", "-", 0U, "-", rows, 1U,
        phase_hex[5]);
    require_delta_failure(seal, &phase_heads[0], &phase_heads[5],
                          "resource mutation while environment is armed is refused");
    rows[0].revision = 3U;
    rows[0].status = HEX4;

    altered = heads[1];
    altered.resources[0].resource_id.bytes[0] = 'x';
    require_delta_failure(seal, &heads[0], &altered,
                          "resource rename is not an advance");
    altered = heads[1];
    altered.resources[0].intent_sha256[0] ^= 1U;
    require_delta_failure(seal, &heads[0], &altered,
                          "intent replacement is not an advance");
    altered = heads[1];
    altered.resources[0].status_revision = 3U;
    require_delta_failure(seal, &heads[0], &altered,
                          "skipped resource revision is refused");
    altered = heads[1];
    memcpy(altered.resources[0].status_sha256,
           heads[0].resources[0].status_sha256,
           DCENT_RECEIPT_SHA256_BYTES);
    require_delta_failure(seal, &heads[0], &altered,
                          "advance must select a new status digest");
    altered = heads[1];
    altered.claim_present = true;
    altered.authority = DCENT_RECEIPT_AUTHORITY_RECONCILER;
    altered.claim_id = heads[2].claim_id;
    altered.authority_id = heads[2].claim_id;
    altered.claim_status_revision = 1U;
    memcpy(altered.claim_intent_sha256, heads[2].claim_intent_sha256,
           DCENT_RECEIPT_SHA256_BYTES);
    memcpy(altered.claim_status_sha256, heads[2].claim_status_sha256,
           DCENT_RECEIPT_SHA256_BYTES);
    require_delta_failure(seal, &heads[0], &altered,
                          "resource advance and claim transfer cannot share a head");
    altered = heads[3];
    altered.claim_present = false;
    altered.authority = DCENT_RECEIPT_AUTHORITY_OWNER;
    altered.authority_id = seal->transaction_id;
    require_delta_failure(seal, &heads[2], &altered,
                          "claim removal and owner restoration are refused");

    altered_seal = *seal;
    altered_seal.record_sha256[0] ^= 1U;
    require_pair_failure(&altered_seal, genesis, genesis,
                         "head pair must authenticate the exact sibling seal");
    altered = *genesis;
    altered.authority_id.bytes[0] = 'x';
    require_pair_failure(seal, &altered, &altered,
                         "owner authority must equal sealed transaction ID");
    altered = *genesis;
    altered.previous_head_sha256[0] = 1U;
    require_pair_failure(seal, &altered, &altered,
                         "absent genesis linkage must have a zero projection");
    require_pair_failure(seal, &heads[0], &heads[0],
                         "equal non-genesis banks are a blocking pair");
    require_pair_failure(seal, &heads[0], genesis,
                         "generation one in even bank violates parity");
    require_pair_failure(seal, &heads[0], &heads[1],
                         "generation two in odd bank violates parity");
    require_pair_failure(seal, genesis, &heads[1],
                         "nonconsecutive banks are a blocking pair");
    altered = heads[0];
    altered.previous_head_sha256[0] ^= 1U;
    require_pair_failure(seal, genesis, &altered,
                         "unlinked newer bank is blocking");
    require(dcent_receipt_storage_validate_manifest_pair_abi2(
                NULL, genesis, genesis, &pair) ==
                DCENT_RECEIPT_STORAGE_INVALID_ARGUMENT,
            "pair validator rejects missing seal");
    require(dcent_receipt_storage_validate_manifest_pair_abi2(
                seal, genesis, genesis, NULL) ==
                DCENT_RECEIPT_STORAGE_INVALID_ARGUMENT,
            "pair validator rejects missing output");
}

int main(void)
{
    struct dcent_receipt_storage_seal seal;
    struct dcent_receipt_storage_head genesis;
    char seal_hex[65];
    char genesis_hex[65];

    test_seal_parser(&seal, seal_hex);
    test_head_parsers_and_limits(&seal, seal_hex, &genesis, genesis_hex);
    require(strlen(genesis_hex) == 64U,
            "genesis test exposes its canonical record digest");
    test_pair_and_delta_validation(&seal, seal_hex, &genesis);
    require(strcmp(dcent_receipt_storage_result_name(
                       DCENT_RECEIPT_STORAGE_SEMANTIC),
                   "semantic") == 0,
            "storage result names are stable");

    printf("dcentos-receipt storage ABI2 tests: %u assertions\n", assertions);
    printf("dcentos-receipt storage ABI2 owned sizes: seal=%zu head=%zu "
           "manifest_pair=%zu delta=%zu\n",
           sizeof(struct dcent_receipt_storage_seal),
           sizeof(struct dcent_receipt_storage_head),
           sizeof(struct dcent_receipt_storage_manifest_pair),
           sizeof(struct dcent_receipt_storage_delta));
    return 0;
}
