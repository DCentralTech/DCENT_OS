/* SPDX-License-Identifier: GPL-3.0-or-later */
#include "receipt_projection.h"

#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define TX_ID "tx.main"
#define BOOT_ID "01234567-89ab-cdef-0123-456789abcdef"
#define OTHER_BOOT_ID "00000000-0000-4000-8000-000000000000"
#define CLAIM_ID "claim1"
#define ZERO_SHA                                                               \
    "0000000000000000000000000000000000000000000000000000000000000000"

static unsigned int assertions;

struct test_context {
    struct dcent_receipt_binding_anchor binding;
    struct dcent_receipt_lock_anchor lock;
    struct dcent_receipt_storage_seal seal;
    struct dcent_receipt_storage_head genesis;
    char binding_hex[65];
    char lock_hex[65];
    char seal_hex[65];
    size_t aggregate_bytes;
};

struct test_row {
    const char *kind;
    const char *id;
    const char *intent;
    unsigned int revision;
    const char *status;
};

struct resource_fixture {
    const char *kind;
    const char *id;
    char intent[4096];
    size_t intent_size;
    char intent_hex[65];
    char statuses[DCENT_RECEIPT_MAX_REVISIONS][4096];
    size_t status_sizes[DCENT_RECEIPT_MAX_REVISIONS];
    char status_hex[DCENT_RECEIPT_MAX_REVISIONS][65];
    size_t revisions;
};

struct claim_fixture {
    char intent[4096];
    size_t intent_size;
    char intent_hex[65];
    char quiescence_hex[65];
    char statuses[DCENT_RECEIPT_MAX_REVISIONS][4096];
    size_t status_sizes[DCENT_RECEIPT_MAX_REVISIONS];
    char status_hex[DCENT_RECEIPT_MAX_REVISIONS][65];
    size_t revisions;
};

struct phase_fixture {
    char statuses[DCENT_RECEIPT_MAX_PHASE_REVISIONS][4096];
    size_t status_sizes[DCENT_RECEIPT_MAX_PHASE_REVISIONS];
    char status_hex[DCENT_RECEIPT_MAX_PHASE_REVISIONS][65];
    size_t revisions;
};

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

    require(*used < capacity, "fixture retains output capacity");
    va_start(arguments, format);
    written = vsnprintf(buffer + *used, capacity - *used, format, arguments);
    va_end(arguments);
    require(written >= 0 && (size_t)written < capacity - *used,
            "fixture output fits");
    *used += (size_t)written;
}

static void digest_hex(const void *data, size_t size, char output[65])
{
    unsigned char digest[DCENT_RECEIPT_SHA256_BYTES];

    dcent_receipt_sha256(digest, data, size);
    dcent_receipt_sha256_hex(output, digest);
}

static void account(struct test_context *context, size_t size)
{
    require(size <= DCENT_RECEIPT_MAX_LEDGER - context->aggregate_bytes,
            "fixture aggregate uses checked addition");
    context->aggregate_bytes += size;
}

static size_t build_lock(char *buffer, size_t capacity)
{
    size_t used = 0U;

    appendf(buffer, capacity, &used,
            "schema=dcentos-sysupgrade-lock-v3\n"
            "transaction_id=" TX_ID "\n"
            "boot_id=" BOOT_ID "\n"
            "pid=123\n"
            "starttime=456\n"
            "owner=zynq-sysupgrade\n");
    return used;
}

static size_t build_binding(char *buffer, size_t capacity,
                            const char *lock_digest)
{
    size_t used = 0U;

    appendf(buffer, capacity, &used,
            "schema=dcentos-sysupgrade-resource-ledger-abi1\n"
            "transaction_id=" TX_ID "\n"
            "boot_id=" BOOT_ID "\n"
            "owner_pid=123\n"
            "owner_starttime=456\n"
            "owner_mount_namespace=1:22\n"
            "acquisition_guard_device_inode=5:9\n"
            "transaction_lock_path=/run/dcentos-sysupgrade.lock\n"
            "transaction_lock_device_inode=5:10\n"
            "transaction_lock_owner_device_inode=5:13\n"
            "transaction_lock_owner_sha256=%s\n"
            "storage_mount_id=42\n"
            "ledger_path=/run/dcentos-sysupgrade.lock/ledger\n"
            "ledger_device_inode=5:11\n"
            "owner=zynq-sysupgrade\n",
            lock_digest);
    return used;
}

static size_t build_seal(char *buffer, size_t capacity,
                         const char *lock_digest,
                         const char *binding_digest)
{
    size_t used = 0U;

    appendf(buffer, capacity, &used,
            "schema=dcentos-sysupgrade-ledger-seal-abi2\n"
            "transaction_id=" TX_ID "\n"
            "boot_id=" BOOT_ID "\n"
            "acquisition_guard_device_inode=5:9\n"
            "transaction_lock_device_inode=5:10\n"
            "transaction_lock_owner_device_inode=5:13\n"
            "transaction_lock_owner_sha256=%s\n"
            "storage_mount_id=42\n"
            "ledger_device_inode=5:11\n"
            "binding_sha256=%s\n"
            "mutation_lease_device_inode=5:12\n"
            "initial_transaction_phase=active\n"
            "owner=zynq-sysupgrade\n",
            lock_digest, binding_digest);
    return used;
}

static size_t build_head(
    char *buffer, size_t capacity, const char *seal_digest,
    unsigned int generation, const struct dcent_receipt_storage_head *previous,
    const char *authority, const char *authority_id, const char *phase,
    unsigned int phase_revision, const char *phase_digest, bool claim_present,
    const char *claim_id, const char *claim_intent,
    unsigned int claim_revision, const char *claim_status,
    const struct test_row *rows, size_t row_count)
{
    char previous_hex[65];
    size_t used = 0U;
    size_t index;

    appendf(buffer, capacity, &used,
            "schema=dcentos-sysupgrade-ledger-head-abi2\n"
            "seal_sha256=%s\n"
            "generation=%u\n",
            seal_digest, generation);
    if (previous == NULL) {
        appendf(buffer, capacity, &used,
                "previous_generation=-\nprevious_head_sha256=-\n");
    } else {
        dcent_receipt_sha256_hex(previous_hex, previous->record_sha256);
        appendf(buffer, capacity, &used,
                "previous_generation=%u\nprevious_head_sha256=%s\n",
                previous->generation, previous_hex);
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

static struct dcent_receipt_storage_head make_head(
    struct test_context *context, unsigned int generation,
    const struct dcent_receipt_storage_head *previous, const char *authority,
    const char *authority_id, const char *phase, unsigned int phase_revision,
    const char *phase_digest, bool claim_present, const char *claim_id,
    const char *claim_intent, unsigned int claim_revision,
    const char *claim_status, const struct test_row *rows, size_t row_count)
{
    char buffer[9000];
    size_t size;
    struct dcent_receipt_storage_head parsed;
    int parse_result;

    size = build_head(buffer, sizeof(buffer), context->seal_hex, generation,
                      previous, authority, authority_id, phase, phase_revision,
                      phase_digest, claim_present, claim_id, claim_intent,
                      claim_revision, claim_status, rows, row_count);
    memset(&parsed, 0, sizeof(parsed));
    parse_result = dcent_receipt_storage_parse_head_abi2(buffer, size, &parsed);
    if (parse_result != DCENT_RECEIPT_STORAGE_OK) {
        fprintf(stderr, "head fixture generation=%u parse=%s\n", generation,
                dcent_receipt_storage_result_name(parse_result));
        fwrite(buffer, 1U, size, stderr);
    }
    require(parse_result == DCENT_RECEIPT_STORAGE_OK,
            "generated storage head parses");
    account(context, size);
    memset(buffer, 0xa5, sizeof(buffer));
    return parsed;
}

static void init_context(struct test_context *context)
{
    char buffer[9000];
    size_t size;
    struct dcent_receipt_lock_owner lock_owner;
    struct dcent_receipt_binding binding;

    memset(context, 0, sizeof(*context));
    size = build_lock(buffer, sizeof(buffer));
    memset(&lock_owner, 0, sizeof(lock_owner));
    require(dcent_receipt_parse_lock_owner_v3(buffer, size, &lock_owner) ==
                DCENT_RECEIPT_FORMAT_OK,
            "fixture lock-v3 parses");
    require(dcent_receipt_lock_anchor_init(&context->lock, &lock_owner) ==
                DCENT_RECEIPT_FORMAT_OK,
            "fixture lock anchor initializes");
    dcent_receipt_sha256_hex(context->lock_hex,
                            context->lock.record_sha256);
    account(context, size);

    size = build_binding(buffer, sizeof(buffer), context->lock_hex);
    memset(&binding, 0, sizeof(binding));
    require(dcent_receipt_parse_binding_abi1(buffer, size, &binding) ==
                DCENT_RECEIPT_FORMAT_OK,
            "fixture binding parses");
    require(dcent_receipt_binding_anchor_init(&context->binding, &binding) ==
                DCENT_RECEIPT_FORMAT_OK,
            "fixture binding anchor initializes");
    dcent_receipt_sha256_hex(context->binding_hex,
                            context->binding.record_sha256);
    account(context, size);

    size = build_seal(buffer, sizeof(buffer), context->lock_hex,
                      context->binding_hex);
    memset(&context->seal, 0, sizeof(context->seal));
    require(dcent_receipt_storage_parse_seal_abi2(
                buffer, size, &context->seal) == DCENT_RECEIPT_STORAGE_OK,
            "fixture storage seal parses");
    dcent_receipt_sha256_hex(context->seal_hex,
                            context->seal.record_sha256);
    account(context, size);
    memset(buffer, 0xa5, sizeof(buffer));

    context->genesis = make_head(
        context, 0U, NULL, "owner", TX_ID, "active", 0U, "-", false, "-",
        "-", 0U, "-", NULL, 0U);
}

static void init_projection(
    struct dcent_receipt_projection *projection,
    const struct test_context *context,
    const struct dcent_receipt_storage_head *previous,
    const struct dcent_receipt_storage_head *current)
{
    const struct dcent_receipt_storage_head *bank0;
    const struct dcent_receipt_storage_head *bank1;
    enum dcent_receipt_projection_result result;
    struct dcent_receipt_storage_manifest_pair pair;
    enum dcent_receipt_storage_result storage_result;

    if (current->generation == 0U) {
        bank0 = current;
        bank1 = current;
    } else if ((current->generation % 2U) == 0U) {
        bank0 = current;
        bank1 = previous;
    } else {
        bank0 = previous;
        bank1 = current;
    }
    result = dcent_receipt_projection_init_abi2(
        projection, &context->binding, &context->lock, &context->seal, bank0,
        bank1);
    if (result != DCENT_RECEIPT_PROJECTION_OK) {
        memset(&pair, 0, sizeof(pair));
        storage_result = dcent_receipt_storage_validate_manifest_pair_abi2(
            &context->seal, bank0, bank1, &pair);
        fprintf(stderr, "projection fixture generation=%u init=%s\n",
                current->generation,
                dcent_receipt_projection_result_name(result));
        fprintf(stderr, "storage pair=%s\n",
                dcent_receipt_storage_result_name(storage_result));
        fprintf(stderr,
                "bank0 generation=%u resources=%zu first_revision=%u "
                "bank1 generation=%u resources=%zu first_revision=%u\n",
                bank0->generation, bank0->resource_count,
                bank0->resource_count == 0U
                    ? 0U
                    : bank0->resources[0].status_revision,
                bank1->generation, bank1->resource_count,
                bank1->resource_count == 0U
                    ? 0U
                    : bank1->resources[0].status_revision);
    }
    require(result == DCENT_RECEIPT_PROJECTION_OK,
            "projection initializes from canonical surviving banks");
}

static size_t build_resource_intent(char *buffer, size_t capacity,
                                    const struct test_context *context,
                                    const char *kind, const char *id,
                                    const char *evidence_type,
                                    const char *body)
{
    char body_hex[65];
    size_t body_size = strlen(body);
    size_t used = 0U;

    digest_hex(body, body_size, body_hex);
    appendf(buffer, capacity, &used,
            "schema=dcentos-sysupgrade-resource-intent-abi1\n"
            "binding_sha256=%s\n"
            "transaction_id=" TX_ID "\n"
            "kind=%s\n"
            "resource_id=%s\n"
            "provenance=created\n"
            "identity_a=7\n"
            "identity_b=1\n"
            "identity_c=-\n"
            "evidence_type=%s\n"
            "evidence_size=%zu\n"
            "evidence_sha256=%s\n\n",
            context->binding_hex, kind, id, evidence_type, body_size,
            body_hex);
    require(body_size < capacity - used, "resource intent body fits");
    memcpy(buffer + used, body, body_size);
    return used + body_size;
}

static size_t build_resource_status(
    char *buffer, size_t capacity, const struct test_context *context,
    const char *kind, const char *id, const char *intent_digest,
    const char *phase, unsigned int revision, unsigned int generation,
    const char *previous_digest, const char *actor_kind, const char *actor_id,
    const char *evidence_type, const char *body)
{
    char body_hex[65];
    const char *body_digest = "-";
    size_t body_size = strlen(body);
    size_t used = 0U;

    if (body_size != 0U) {
        digest_hex(body, body_size, body_hex);
        body_digest = body_hex;
    }
    appendf(buffer, capacity, &used,
            "schema=dcentos-sysupgrade-resource-status-abi1\n"
            "binding_sha256=%s\n"
            "transaction_id=" TX_ID "\n"
            "kind=%s\n"
            "resource_id=%s\n"
            "intent_sha256=%s\n"
            "phase=%s\n"
            "revision=%u\n"
            "ledger_generation=%u\n"
            "previous_status_sha256=%s\n"
            "actor_kind=%s\n"
            "actor_id=%s\n"
            "evidence_type=%s\n"
            "evidence_size=%zu\n"
            "evidence_sha256=%s\n\n",
            context->binding_hex, kind, id, intent_digest, phase, revision,
            generation, previous_digest, actor_kind, actor_id, evidence_type,
            body_size, body_digest);
    require(body_size < capacity - used, "resource status body fits");
    memcpy(buffer + used, body, body_size);
    return used + body_size;
}

static void prepare_attachment(struct test_context *context,
                               struct resource_fixture *resource,
                               const char *id)
{
    memset(resource, 0, sizeof(*resource));
    resource->kind = "attachment";
    resource->id = id;
    resource->intent_size = build_resource_intent(
        resource->intent, sizeof(resource->intent), context, resource->kind,
        resource->id, "attachment-intent-v1", "absent=true\n");
    digest_hex(resource->intent, resource->intent_size, resource->intent_hex);
    account(context, resource->intent_size);
}

static void append_attachment_status(
    struct test_context *context, struct resource_fixture *resource,
    const char *phase, unsigned int generation, const char *actor_kind,
    const char *actor_id)
{
    const char *evidence_type;
    const char *body;
    const char *previous;
    size_t index = resource->revisions;

    require(index < DCENT_RECEIPT_MAX_REVISIONS,
            "resource fixture revision fits");
    if (strcmp(phase, "pending") == 0) {
        evidence_type = "-";
        body = "";
    } else if (strcmp(phase, "active") == 0) {
        evidence_type = "attachment-active-v1";
        body = "attached=true\n";
    } else if (strcmp(phase, "release-pending") == 0) {
        evidence_type = "attachment-release-pending-v1";
        body = "same_object=true\n";
    } else if (strcmp(phase, "released") == 0) {
        evidence_type = "attachment-released-v1";
        body = "absent=true\n";
    } else {
        evidence_type = "attachment-conflict-v1";
        body = "conflict=true\n";
    }
    previous = index == 0U ? "-" : resource->status_hex[index - 1U];
    resource->status_sizes[index] = build_resource_status(
        resource->statuses[index], sizeof(resource->statuses[index]), context,
        resource->kind, resource->id, resource->intent_hex, phase,
        (unsigned int)index + 1U, generation, previous, actor_kind, actor_id,
        evidence_type, body);
    digest_hex(resource->statuses[index], resource->status_sizes[index],
               resource->status_hex[index]);
    account(context, resource->status_sizes[index]);
    resource->revisions++;
}

static enum dcent_receipt_projection_result ingest_resource(
    struct dcent_receipt_projection *projection,
    struct resource_fixture *resource, size_t *slot_out)
{
    struct dcent_receipt_resource_intent intent;
    struct dcent_receipt_resource_status status;
    enum dcent_receipt_projection_result result;
    size_t index;
    size_t slot = 0U;

    memset(&intent, 0, sizeof(intent));
    require(dcent_receipt_parse_resource_intent_abi1(
                resource->intent, resource->intent_size, &intent) ==
                DCENT_RECEIPT_FORMAT_OK,
            "resource fixture parses before ingestion");
    result = dcent_receipt_projection_resource_begin(projection, &intent,
                                                     &slot);
    if (result != DCENT_RECEIPT_PROJECTION_OK)
        return result;
    memset(resource->intent, 0xa5, sizeof(resource->intent));
    for (index = 0U; index < resource->revisions; index++) {
        memset(&status, 0, sizeof(status));
        require(dcent_receipt_parse_resource_status_abi1(
                    resource->statuses[index], resource->status_sizes[index],
                    &status) == DCENT_RECEIPT_FORMAT_OK,
                "resource status fixture parses before ingestion");
        result = dcent_receipt_projection_resource_add(projection, slot,
                                                       &status);
        if (result != DCENT_RECEIPT_PROJECTION_OK)
            return result;
        memset(resource->statuses[index], 0xa5,
               sizeof(resource->statuses[index]));
    }
    result = dcent_receipt_projection_resource_finish(projection, slot);
    if (result == DCENT_RECEIPT_PROJECTION_OK && slot_out != NULL)
        *slot_out = slot;
    return result;
}

static size_t build_claim_intent(char *buffer, size_t capacity,
                                 const struct test_context *context,
                                 const char *reconciler_boot_id)
{
    static const char body[] = "owner_dead=true\n";
    char body_hex[65];
    size_t used = 0U;

    digest_hex(body, sizeof(body) - 1U, body_hex);
    appendf(buffer, capacity, &used,
            "schema=dcentos-sysupgrade-reconcile-intent-abi1\n"
            "binding_sha256=%s\n"
            "transaction_id=" TX_ID "\n"
            "claim_id=" CLAIM_ID "\n"
            "reconciler_boot_id=%s\n"
            "reconciler_pid=321\n"
            "reconciler_starttime=987654\n"
            "reconciler_mount_namespace=3:77\n"
            "maintenance_lock_path=/run/dcentos-maintenance.lock\n"
            "maintenance_lock_device_inode=4:88\n"
            "owner=zynq-sysupgrade-reconciler\n"
            "evidence_type=owner-death-v1\n"
            "evidence_size=%zu\n"
            "evidence_sha256=%s\n\n",
            context->binding_hex, reconciler_boot_id, sizeof(body) - 1U,
            body_hex);
    require(sizeof(body) - 1U < capacity - used,
            "claim intent body fits");
    memcpy(buffer + used, body, sizeof(body) - 1U);
    return used + sizeof(body) - 1U;
}

static void prepare_claim(struct test_context *context,
                          struct claim_fixture *claim,
                          const char *reconciler_boot_id)
{
    memset(claim, 0, sizeof(*claim));
    claim->intent_size = build_claim_intent(
        claim->intent, sizeof(claim->intent), context, reconciler_boot_id);
    digest_hex(claim->intent, claim->intent_size, claim->intent_hex);
    digest_hex("quiescent=true\n", strlen("quiescent=true\n"),
               claim->quiescence_hex);
    account(context, claim->intent_size);
}

static size_t build_claim_status(
    char *buffer, size_t capacity, const char *intent_digest,
    const char *phase, unsigned int revision, unsigned int generation,
    const char *previous_digest, const char *quiescence_digest,
    const char *evidence_type, const char *body)
{
    char body_hex[65];
    const char *body_digest = "-";
    size_t body_size = strlen(body);
    size_t used = 0U;

    if (body_size != 0U) {
        digest_hex(body, body_size, body_hex);
        body_digest = body_hex;
    }
    appendf(buffer, capacity, &used,
            "schema=dcentos-sysupgrade-reconcile-status-abi1\n"
            "claim_intent_sha256=%s\n"
            "phase=%s\n"
            "revision=%u\n"
            "ledger_generation=%u\n"
            "previous_status_sha256=%s\n"
            "actor_id=" CLAIM_ID "\n"
            "quiescence_sha256=%s\n"
            "evidence_type=%s\n"
            "evidence_size=%zu\n"
            "evidence_sha256=%s\n\n",
            intent_digest, phase, revision, generation, previous_digest,
            quiescence_digest, evidence_type, body_size, body_digest);
    require(body_size < capacity - used, "claim status body fits");
    memcpy(buffer + used, body, body_size);
    return used + body_size;
}

static void append_claim_status(struct test_context *context,
                                struct claim_fixture *claim,
                                const char *phase,
                                unsigned int generation)
{
    const char *quiescence = "-";
    const char *evidence_type;
    const char *body;
    const char *previous;
    size_t index = claim->revisions;

    require(index < DCENT_RECEIPT_MAX_REVISIONS,
            "claim fixture revision fits");
    if (strcmp(phase, "claimed") == 0) {
        evidence_type = "-";
        body = "";
    } else if (strcmp(phase, "quiescent") == 0) {
        quiescence = claim->quiescence_hex;
        evidence_type = "maintenance-quiescence-v1";
        body = "quiescent=true\n";
    } else if (strcmp(phase, "reconciling") == 0) {
        quiescence = claim->quiescence_hex;
        evidence_type = "reconciliation-begin-v1";
        body = "admitted=true\n";
    } else if (strcmp(phase, "complete") == 0) {
        quiescence = claim->quiescence_hex;
        evidence_type = "reconciliation-complete-v1";
        body = "released=true\n";
    } else {
        if (index > 1U)
            quiescence = claim->quiescence_hex;
        evidence_type = "reconciliation-blocked-v1";
        body = "reason=conflict\n";
    }
    previous = index == 0U ? "-" : claim->status_hex[index - 1U];
    claim->status_sizes[index] = build_claim_status(
        claim->statuses[index], sizeof(claim->statuses[index]),
        claim->intent_hex, phase, (unsigned int)index + 1U, generation,
        previous, quiescence, evidence_type, body);
    digest_hex(claim->statuses[index], claim->status_sizes[index],
               claim->status_hex[index]);
    account(context, claim->status_sizes[index]);
    claim->revisions++;
}

static enum dcent_receipt_projection_result ingest_claim(
    struct dcent_receipt_projection *projection, struct claim_fixture *claim)
{
    struct dcent_receipt_claim_intent intent;
    struct dcent_receipt_claim_status status;
    enum dcent_receipt_projection_result result;
    size_t index;

    memset(&intent, 0, sizeof(intent));
    require(dcent_receipt_parse_claim_intent_abi1(
                claim->intent, claim->intent_size, &intent) ==
                DCENT_RECEIPT_FORMAT_OK,
            "claim fixture parses before ingestion");
    result = dcent_receipt_projection_claim_begin(projection, &intent);
    if (result != DCENT_RECEIPT_PROJECTION_OK)
        return result;
    memset(claim->intent, 0xa5, sizeof(claim->intent));
    for (index = 0U; index < claim->revisions; index++) {
        memset(&status, 0, sizeof(status));
        require(dcent_receipt_parse_claim_status_abi1(
                    claim->statuses[index], claim->status_sizes[index],
                    &status) == DCENT_RECEIPT_FORMAT_OK,
                "claim status fixture parses before ingestion");
        result = dcent_receipt_projection_claim_add(projection, &status);
        if (result != DCENT_RECEIPT_PROJECTION_OK)
            return result;
        memset(claim->statuses[index], 0xa5,
               sizeof(claim->statuses[index]));
    }
    return dcent_receipt_projection_claim_finish(projection);
}

static size_t build_phase_status(
    char *buffer, size_t capacity, const struct test_context *context,
    const char *phase, unsigned int revision, unsigned int generation,
    const char *previous_digest, const char *actor_kind, const char *actor_id,
    const char *evidence_type, const char *body)
{
    char body_hex[65];
    size_t body_size = strlen(body);
    size_t used = 0U;

    digest_hex(body, body_size, body_hex);
    appendf(buffer, capacity, &used,
            "schema=dcentos-sysupgrade-transaction-phase-status-abi2\n"
            "binding_sha256=%s\n"
            "transaction_id=" TX_ID "\n"
            "phase=%s\n"
            "revision=%u\n"
            "ledger_generation=%u\n"
            "previous_status_sha256=%s\n"
            "actor_kind=%s\n"
            "actor_id=%s\n"
            "evidence_type=%s\n"
            "evidence_size=%zu\n"
            "evidence_sha256=%s\n\n",
            context->binding_hex, phase, revision, generation,
            previous_digest, actor_kind, actor_id, evidence_type, body_size,
            body_hex);
    require(body_size < capacity - used, "phase status body fits");
    memcpy(buffer + used, body, body_size);
    return used + body_size;
}

static void append_phase(struct test_context *context,
                         struct phase_fixture *phase_fixture,
                         const char *phase, unsigned int generation,
                         const char *actor_kind, const char *actor_id)
{
    const char *evidence_type;
    const char *body;
    const char *previous;
    size_t index = phase_fixture->revisions;

    require(index < DCENT_RECEIPT_MAX_PHASE_REVISIONS,
            "phase fixture revision fits");
    if (strcmp(phase, "env-commit-armed") == 0) {
        evidence_type = "transaction-env-commit-armed-v1";
        body = "environment_digest=verified\n";
    } else if (strcmp(phase, "env-committed") == 0) {
        evidence_type = "transaction-env-committed-v1";
        body = "commit_verified=true\n";
    } else if (strcmp(phase, "cleanup-required") == 0) {
        evidence_type = "transaction-cleanup-required-v1";
        body = "cleanup_required=true\n";
    } else {
        evidence_type = "transaction-env-commit-disarmed-v1";
        body = "disarm_verified=true\n";
    }
    previous = index == 0U ? "-" : phase_fixture->status_hex[index - 1U];
    phase_fixture->status_sizes[index] = build_phase_status(
        phase_fixture->statuses[index],
        sizeof(phase_fixture->statuses[index]), context, phase,
        (unsigned int)index + 1U, generation, previous, actor_kind, actor_id,
        evidence_type, body);
    digest_hex(phase_fixture->statuses[index],
               phase_fixture->status_sizes[index],
               phase_fixture->status_hex[index]);
    account(context, phase_fixture->status_sizes[index]);
    phase_fixture->revisions++;
}

static enum dcent_receipt_projection_result ingest_phase(
    struct dcent_receipt_projection *projection,
    struct phase_fixture *phase_fixture)
{
    struct dcent_receipt_transaction_phase_status status;
    enum dcent_receipt_projection_result result;
    size_t index;

    for (index = 0U; index < phase_fixture->revisions; index++) {
        memset(&status, 0, sizeof(status));
        require(dcent_receipt_parse_transaction_phase_status_abi2(
                    phase_fixture->statuses[index],
                    phase_fixture->status_sizes[index], &status) ==
                    DCENT_RECEIPT_FORMAT_OK,
                "phase fixture parses before ingestion");
        result = dcent_receipt_projection_phase_add(projection, &status);
        if (result != DCENT_RECEIPT_PROJECTION_OK)
            return result;
        memset(phase_fixture->statuses[index], 0xa5,
               sizeof(phase_fixture->statuses[index]));
    }
    return dcent_receipt_projection_phase_finish(projection);
}

static void require_finalize_failure_atomic(
    const struct dcent_receipt_projection *projection, size_t aggregate_bytes,
    enum dcent_receipt_projection_result expected, const char *message)
{
    struct dcent_receipt_projection_summary output;
    struct dcent_receipt_projection_summary before;

    memset(&output, 0xa5, sizeof(output));
    before = output;
    require(dcent_receipt_projection_finalize_abi2(
                projection, aggregate_bytes, &output) == expected,
            message);
    require(memcmp(&output, &before, sizeof(output)) == 0,
            "failed projection finalization leaves output untouched");
}

static void test_genesis_and_api_hardening(void)
{
    static struct test_context context;
    static struct dcent_receipt_projection projection;
    static struct dcent_receipt_projection before;
    static struct dcent_receipt_projection corrupt;
    struct dcent_receipt_projection_summary summary;

    init_context(&context);
    memset(&projection, 0xa5, sizeof(projection));
    before = projection;
    require(dcent_receipt_projection_init_abi2(
                &projection, NULL, &context.lock, &context.seal,
                &context.genesis, &context.genesis) ==
                DCENT_RECEIPT_PROJECTION_INVALID_ARGUMENT,
            "projection init classifies a null binding as invalid argument");
    require(memcmp(&projection, &before, sizeof(projection)) == 0,
            "failed projection init is output-failure-atomic");

    init_projection(&projection, &context, &context.genesis,
                    &context.genesis);
    require(sizeof(projection) <= DCENT_RECEIPT_PROJECTION_MAX_STATE_BYTES,
            "projection state remains within deliberate embedded budget");
    require_finalize_failure_atomic(
        &projection, context.aggregate_bytes, DCENT_RECEIPT_PROJECTION_FORMAT,
        "unfinished phase chain refuses genesis finalization");
    require(dcent_receipt_projection_phase_finish(&projection) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "zero-revision active phase chain finishes");
    memset(&summary, 0, sizeof(summary));
    require(dcent_receipt_projection_finalize_abi2(
                &projection, context.aggregate_bytes, &summary) ==
                DCENT_RECEIPT_PROJECTION_OK &&
                summary.initialized && summary.generation == 0U &&
                summary.event_count == 0U && summary.resource_count == 0U &&
                !summary.claim_present &&
                summary.authority == DCENT_RECEIPT_AUTHORITY_OWNER &&
                summary.transaction_phase == DCENT_RECEIPT_LOCK_ACTIVE,
            "canonical genesis projects deterministic empty state");
    require_finalize_failure_atomic(
        &projection, DCENT_RECEIPT_MAX_LEDGER + 1U,
        DCENT_RECEIPT_PROJECTION_LIMIT,
        "aggregate byte limit is enforced before projection");

    before = projection;
    require(dcent_receipt_projection_init_abi2(
                &projection, &projection.binding, &projection.lock,
                &projection.seal, &projection.banks[0],
                &projection.banks[1]) == DCENT_RECEIPT_PROJECTION_OK,
            "explicit in-place projection reinitialization is alias-safe");
    require(projection.binding.initialized && projection.lock.initialized &&
                projection.seal.initialized,
            "alias-safe reinit retains copied anchors");
    require(memcmp(&projection.binding, &before.binding,
                   sizeof(projection.binding)) == 0 &&
                memcmp(&projection.lock, &before.lock,
                       sizeof(projection.lock)) == 0 &&
                memcmp(&projection.seal, &before.seal,
                       sizeof(projection.seal)) == 0 &&
                memcmp(projection.banks, before.banks,
                       sizeof(projection.banks)) == 0 &&
                memcmp(&projection.pair, &before.pair,
                       sizeof(projection.pair)) == 0,
            "alias-safe reinit preserves exact admitted identities");
    require(dcent_receipt_projection_phase_finish(&projection) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "reinitialized phase chain finishes");
    require(dcent_receipt_projection_finalize_abi2(
                &projection, DCENT_RECEIPT_MAX_LEDGER, &summary) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "exact aggregate byte limit remains admissible");

    corrupt = projection;
    corrupt.pair.current_generation =
        DCENT_RECEIPT_MAX_LEDGER_GENERATION + 1U;
    require_finalize_failure_atomic(
        &corrupt, context.aggregate_bytes, DCENT_RECEIPT_PROJECTION_FORMAT,
        "tampered stored generation is rejected without event OOB");
    corrupt = projection;
    corrupt.pair.current_bank = 2U;
    require_finalize_failure_atomic(
        &corrupt, context.aggregate_bytes, DCENT_RECEIPT_PROJECTION_FORMAT,
        "tampered stored bank index is rejected without bank OOB");
    corrupt = projection;
    corrupt.resource_count = DCENT_RECEIPT_MAX_RESOURCES + 1U;
    require_finalize_failure_atomic(
        &corrupt, context.aggregate_bytes, DCENT_RECEIPT_PROJECTION_FORMAT,
        "tampered resource count is rejected before resource traversal");
}

static void test_resource_projection_and_manifest_mismatch(void)
{
    static struct test_context context;
    static struct resource_fixture resource;
    static struct dcent_receipt_projection projection;
    struct dcent_receipt_projection_summary summary;
    struct dcent_receipt_storage_head predecessor;
    struct dcent_receipt_storage_head older;
    struct dcent_receipt_storage_head current;
    struct dcent_receipt_storage_head mismatched;
    struct test_row row;

    init_context(&context);
    prepare_attachment(&context, &resource, "attach-a");
    append_attachment_status(&context, &resource, "pending", 1U, "owner",
                             TX_ID);
    row.kind = resource.kind;
    row.id = resource.id;
    row.intent = resource.intent_hex;
    row.revision = 1U;
    row.status = resource.status_hex[0];
    current = make_head(&context, 1U, &context.genesis, "owner", TX_ID,
                        "active", 0U, "-", false, "-", "-", 0U, "-",
                        &row, 1U);
    init_projection(&projection, &context, &context.genesis, &current);
    require(ingest_resource(&projection, &resource, NULL) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "parser-backed resource history ingests");
    require(dcent_receipt_projection_phase_finish(&projection) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "resource scenario phase chain finishes");
    memset(&summary, 0, sizeof(summary));
    require(dcent_receipt_projection_finalize_abi2(
                &projection, context.aggregate_bytes, &summary) ==
                DCENT_RECEIPT_PROJECTION_OK &&
                summary.generation == 1U && summary.event_count == 1U &&
                summary.resource_count == 1U,
            "resource event reconstructs the current manifest");

    init_context(&context);
    prepare_attachment(&context, &resource, "attach-a");
    append_attachment_status(&context, &resource, "pending", 1U, "owner",
                             TX_ID);
    row = (struct test_row){resource.kind, resource.id, resource.intent_hex,
                            1U, ZERO_SHA};
    mismatched = make_head(&context, 1U, &context.genesis, "owner", TX_ID,
                           "active", 0U, "-", false, "-", "-", 0U, "-",
                           &row, 1U);
    init_projection(&projection, &context, &context.genesis, &mismatched);
    require(ingest_resource(&projection, &resource, NULL) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "history with independently mismatched head still ingests");
    require(dcent_receipt_projection_phase_finish(&projection) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "mismatch scenario phase chain finishes");
    require_finalize_failure_atomic(
        &projection, context.aggregate_bytes,
        DCENT_RECEIPT_PROJECTION_MANIFEST_MISMATCH,
        "status history must match the surviving head projection");

    init_context(&context);
    prepare_attachment(&context, &resource, "attach-a");
    append_attachment_status(&context, &resource, "pending", 1U, "owner",
                             TX_ID);
    append_attachment_status(&context, &resource, "active", 2U, "owner",
                             TX_ID);
    append_attachment_status(&context, &resource, "release-pending", 3U,
                             "owner", TX_ID);
    row = (struct test_row){resource.kind, resource.id, resource.intent_hex,
                            1U, resource.status_hex[0]};
    predecessor = make_head(&context, 1U, &context.genesis, "owner", TX_ID,
                            "active", 0U, "-", false, "-", "-", 0U, "-",
                            &row, 1U);
    row.revision = 2U;
    row.status = resource.status_hex[0];
    older = make_head(&context, 2U, &predecessor, "owner", TX_ID, "active",
                      0U, "-", false, "-", "-", 0U, "-", &row, 1U);
    row.revision = 3U;
    row.status = resource.status_hex[2];
    current = make_head(&context, 3U, &older, "owner", TX_ID, "active", 0U,
                        "-", false, "-", "-", 0U, "-", &row, 1U);
    init_projection(&projection, &context, &older, &current);
    require(ingest_resource(&projection, &resource, NULL) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "history for previous-head mismatch ingests");
    require(dcent_receipt_projection_phase_finish(&projection) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "previous-head mismatch phase chain finishes");
    require_finalize_failure_atomic(
        &projection, context.aggregate_bytes,
        DCENT_RECEIPT_PROJECTION_MANIFEST_MISMATCH,
        "history must reconstruct the surviving previous head projection");
}

static void test_environment_commit_requires_released_prefix(void)
{
    static struct test_context context;
    static struct resource_fixture resource;
    static struct phase_fixture phases;
    static struct dcent_receipt_projection projection;
    struct dcent_receipt_storage_head prefix[4];
    struct dcent_receipt_storage_head older;
    struct dcent_receipt_storage_head current;
    struct dcent_receipt_projection_summary summary;
    struct test_row row;

    init_context(&context);
    memset(&phases, 0, sizeof(phases));
    prepare_attachment(&context, &resource, "attach-a");
    append_attachment_status(&context, &resource, "pending", 1U, "owner",
                             TX_ID);
    append_attachment_status(&context, &resource, "active", 2U, "owner",
                             TX_ID);
    append_phase(&context, &phases, "env-commit-armed", 3U, "owner", TX_ID);
    row = (struct test_row){resource.kind, resource.id, resource.intent_hex,
                            1U, resource.status_hex[0]};
    prefix[0] = make_head(&context, 1U, &context.genesis, "owner", TX_ID,
                          "active", 0U, "-", false, "-", "-", 0U, "-",
                          &row, 1U);
    row.revision = 2U;
    row.status = resource.status_hex[1];
    older = make_head(&context, 2U, &prefix[0], "owner", TX_ID, "active",
                      0U, "-", false, "-", "-", 0U, "-", &row, 1U);
    current = make_head(&context, 3U, &older, "owner", TX_ID,
                        "env-commit-armed", 1U, phases.status_hex[0], false,
                        "-", "-", 0U, "-", &row, 1U);
    init_projection(&projection, &context, &older, &current);
    require(ingest_resource(&projection, &resource, NULL) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "active-resource chronology ingests by resource directory");
    require(ingest_phase(&projection, &phases) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "armed phase record ingests independently of chronology");
    require_finalize_failure_atomic(
        &projection, context.aggregate_bytes,
        DCENT_RECEIPT_PROJECTION_CHRONOLOGY,
        "environment commit cannot arm with an active resource");

    init_context(&context);
    memset(&phases, 0, sizeof(phases));
    prepare_attachment(&context, &resource, "attach-a");
    append_attachment_status(&context, &resource, "pending", 1U, "owner",
                             TX_ID);
    append_attachment_status(&context, &resource, "active", 2U, "owner",
                             TX_ID);
    append_attachment_status(&context, &resource, "release-pending", 3U,
                             "owner", TX_ID);
    append_attachment_status(&context, &resource, "released", 4U, "owner",
                             TX_ID);
    append_phase(&context, &phases, "env-commit-armed", 5U, "owner", TX_ID);
    append_phase(&context, &phases, "env-committed", 6U, "owner", TX_ID);
    row = (struct test_row){resource.kind, resource.id, resource.intent_hex,
                            1U, resource.status_hex[0]};
    prefix[0] = make_head(&context, 1U, &context.genesis, "owner", TX_ID,
                          "active", 0U, "-", false, "-", "-", 0U, "-",
                          &row, 1U);
    row.revision = 2U;
    row.status = resource.status_hex[1];
    prefix[1] = make_head(&context, 2U, &prefix[0], "owner", TX_ID,
                          "active", 0U, "-", false, "-", "-", 0U, "-",
                          &row, 1U);
    row.revision = 3U;
    row.status = resource.status_hex[2];
    prefix[2] = make_head(&context, 3U, &prefix[1], "owner", TX_ID,
                          "active", 0U, "-", false, "-", "-", 0U, "-",
                          &row, 1U);
    row.revision = 4U;
    row.status = resource.status_hex[3];
    prefix[3] = make_head(&context, 4U, &prefix[2], "owner", TX_ID,
                          "active", 0U, "-", false, "-", "-", 0U, "-",
                          &row, 1U);
    older = make_head(&context, 5U, &prefix[3], "owner", TX_ID,
                      "env-commit-armed", 1U, phases.status_hex[0], false,
                      "-", "-", 0U, "-", &row, 1U);
    current = make_head(&context, 6U, &older, "owner", TX_ID,
                        "env-committed", 2U, phases.status_hex[1], false,
                        "-", "-", 0U, "-", &row, 1U);
    init_projection(&projection, &context, &older, &current);
    require(ingest_phase(&projection, &phases) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "phase-directory-first scan order ingests");
    require(ingest_resource(&projection, &resource, NULL) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "resource-directory-second scan order ingests");
    memset(&summary, 0, sizeof(summary));
    require(dcent_receipt_projection_finalize_abi2(
                &projection, context.aggregate_bytes, &summary) ==
                DCENT_RECEIPT_PROJECTION_OK &&
                summary.transaction_phase ==
                    DCENT_RECEIPT_LOCK_ENV_COMMITTED,
            "fully released resource may arm and commit environment");

    init_context(&context);
    memset(&phases, 0, sizeof(phases));
    prepare_attachment(&context, &resource, "attach-future");
    append_phase(&context, &phases, "env-commit-armed", 1U, "owner", TX_ID);
    append_phase(&context, &phases, "active", 2U, "owner", TX_ID);
    append_attachment_status(&context, &resource, "pending", 3U, "owner",
                             TX_ID);
    older = make_head(&context, 1U, &context.genesis, "owner", TX_ID,
                      "env-commit-armed", 1U, phases.status_hex[0], false,
                      "-", "-", 0U, "-", NULL, 0U);
    older = make_head(&context, 2U, &older, "owner", TX_ID, "active", 2U,
                      phases.status_hex[1], false, "-", "-", 0U, "-", NULL,
                      0U);
    row = (struct test_row){resource.kind, resource.id, resource.intent_hex,
                            1U, resource.status_hex[0]};
    current = make_head(&context, 3U, &older, "owner", TX_ID, "active", 2U,
                        phases.status_hex[1], false, "-", "-", 0U, "-",
                        &row, 1U);
    init_projection(&projection, &context, &older, &current);
    require(ingest_resource(&projection, &resource, NULL) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "resource created after disarm ingests before phase directory");
    require(ingest_phase(&projection, &phases) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "arm/disarm history ingests after resource directory");
    require(dcent_receipt_projection_finalize_abi2(
                &projection, context.aggregate_bytes, &summary) ==
                DCENT_RECEIPT_PROJECTION_OK && summary.generation == 3U &&
                summary.resource_count == 1U,
            "arm guard ignores resources that do not exist at its prefix");
}

static void test_claim_boot_binding_and_generation_guards(void)
{
    static struct test_context context;
    static struct claim_fixture claim;
    static struct resource_fixture resource_a;
    static struct resource_fixture resource_b;
    static struct dcent_receipt_projection projection;
    static struct dcent_receipt_projection before;
    struct dcent_receipt_claim_intent intent;
    struct dcent_receipt_storage_head older;
    struct dcent_receipt_storage_head current;
    struct test_row rows[2];

    init_context(&context);
    init_projection(&projection, &context, &context.genesis,
                    &context.genesis);
    prepare_claim(&context, &claim, OTHER_BOOT_ID);
    memset(&intent, 0, sizeof(intent));
    require(dcent_receipt_parse_claim_intent_abi1(
                claim.intent, claim.intent_size, &intent) ==
                DCENT_RECEIPT_FORMAT_OK,
            "cross-boot claim intent remains syntactically parseable");
    before = projection;
    require(dcent_receipt_projection_claim_begin(&projection, &intent) ==
                DCENT_RECEIPT_PROJECTION_FORMAT,
            "projection refuses a reconciler from another boot");
    require(memcmp(&projection, &before, sizeof(projection)) == 0,
            "cross-boot claim refusal preserves projection state");

    init_context(&context);
    prepare_attachment(&context, &resource_a, "attach-a");
    append_attachment_status(&context, &resource_a, "pending", 2U, "owner",
                             TX_ID);
    rows[0] = (struct test_row){"attachment", "attach-0", ZERO_SHA, 1U,
                                ZERO_SHA};
    older = make_head(&context, 1U, &context.genesis, "owner", TX_ID,
                      "active", 0U, "-", false, "-", "-", 0U, "-",
                      rows, 1U);
    rows[1] = (struct test_row){resource_a.kind, resource_a.id,
                                resource_a.intent_hex, 1U,
                                resource_a.status_hex[0]};
    current = make_head(&context, 2U, &older, "owner", TX_ID, "active", 0U,
                        "-", false, "-", "-", 0U, "-", rows, 2U);
    init_projection(&projection, &context, &older, &current);
    require(ingest_resource(&projection, &resource_a, NULL) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "gap fixture resource ingests");
    require(dcent_receipt_projection_phase_finish(&projection) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "gap fixture phase chain finishes");
    require_finalize_failure_atomic(
        &projection, context.aggregate_bytes,
        DCENT_RECEIPT_PROJECTION_GENERATION_GAP,
        "missing generation one is rejected before manifest projection");

    init_context(&context);
    prepare_attachment(&context, &resource_a, "attach-a");
    append_attachment_status(&context, &resource_a, "pending", 1U, "owner",
                             TX_ID);
    append_attachment_status(&context, &resource_a, "active", 2U, "owner",
                             TX_ID);
    rows[0] = (struct test_row){resource_a.kind, resource_a.id,
                                resource_a.intent_hex, 1U,
                                resource_a.status_hex[0]};
    older = make_head(&context, 1U, &context.genesis, "owner", TX_ID,
                      "active", 0U, "-", false, "-", "-", 0U, "-", rows,
                      1U);
    rows[0].revision = 2U;
    rows[0].status = resource_a.status_hex[1];
    current = make_head(&context, 2U, &older, "owner", TX_ID, "active", 0U,
                        "-", false, "-", "-", 0U, "-", rows, 1U);
    init_projection(&projection, &context, &older, &current);
    require(ingest_resource(&projection, &resource_a, NULL) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "complete first resource occupies generations one and two");
    prepare_attachment(&context, &resource_b, "attach-b");
    append_attachment_status(&context, &resource_b, "pending", 1U, "owner",
                             TX_ID);
    before = projection;
    require(ingest_resource(&projection, &resource_b, NULL) ==
                DCENT_RECEIPT_PROJECTION_DUPLICATE_GENERATION,
            "second chain cannot reuse an occupied generation");
    require(projection.events[1].kind == before.events[1].kind &&
                projection.events[1].object_index ==
                    before.events[1].object_index &&
                projection.events[1].revision == before.events[1].revision,
            "duplicate generation cannot replace the admitted event");
}

static void test_valid_takeover_cleanup_and_completion(void)
{
    static struct test_context context;
    static struct resource_fixture resource;
    static struct claim_fixture claim;
    static struct phase_fixture phases;
    static struct dcent_receipt_projection projection;
    struct dcent_receipt_storage_head predecessor;
    struct dcent_receipt_storage_head older;
    struct dcent_receipt_storage_head current;
    struct dcent_receipt_projection_summary summary;
    struct test_row row;

    init_context(&context);
    memset(&phases, 0, sizeof(phases));
    prepare_attachment(&context, &resource, "attach-a");
    append_attachment_status(&context, &resource, "pending", 1U, "owner",
                             TX_ID);
    append_attachment_status(&context, &resource, "active", 2U, "owner",
                             TX_ID);
    prepare_claim(&context, &claim, BOOT_ID);
    append_claim_status(&context, &claim, "claimed", 3U);
    append_claim_status(&context, &claim, "quiescent", 4U);
    append_claim_status(&context, &claim, "reconciling", 5U);
    append_phase(&context, &phases, "cleanup-required", 6U, "reconciler",
                 CLAIM_ID);
    append_attachment_status(&context, &resource, "release-pending", 7U,
                             "reconciler", CLAIM_ID);
    append_attachment_status(&context, &resource, "released", 8U,
                             "reconciler", CLAIM_ID);
    append_claim_status(&context, &claim, "complete", 9U);

    memset(&predecessor, 0, sizeof(predecessor));
    predecessor.generation = 6U;
    memset(predecessor.record_sha256, 0x5a,
           sizeof(predecessor.record_sha256));
    row = (struct test_row){resource.kind, resource.id, resource.intent_hex,
                            3U, resource.status_hex[2]};
    predecessor = make_head(
        &context, 7U, &predecessor, "reconciler", CLAIM_ID,
        "cleanup-required", 1U, phases.status_hex[0], true, CLAIM_ID,
        claim.intent_hex, 3U, claim.status_hex[2], &row, 1U);
    row.revision = 4U;
    row.status = resource.status_hex[3];
    older = make_head(&context, 8U, &predecessor, "reconciler", CLAIM_ID,
                      "cleanup-required", 1U, phases.status_hex[0], true,
                      CLAIM_ID, claim.intent_hex, 3U, claim.status_hex[2],
                      &row, 1U);
    current = make_head(&context, 9U, &older, "reconciler", CLAIM_ID,
                        "cleanup-required", 1U, phases.status_hex[0], true,
                        CLAIM_ID, claim.intent_hex, 4U,
                        claim.status_hex[3], &row, 1U);
    init_projection(&projection, &context, &older, &current);
    require(ingest_phase(&projection, &phases) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "phase-first takeover scan ingests");
    require(ingest_resource(&projection, &resource, NULL) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "resource-second takeover scan ingests");
    require(ingest_claim(&projection, &claim) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "claim-last takeover scan ingests");
    memset(&summary, 0, sizeof(summary));
    require(dcent_receipt_projection_finalize_abi2(
                &projection, context.aggregate_bytes, &summary) ==
                DCENT_RECEIPT_PROJECTION_OK && summary.generation == 9U &&
                summary.event_count == 9U && summary.claim_present &&
                summary.authority == DCENT_RECEIPT_AUTHORITY_RECONCILER &&
                summary.transaction_phase ==
                    DCENT_RECEIPT_LOCK_CLEANUP_REQUIRED,
            "takeover cleanup release and completion form one chronology");
}

static void test_cross_chain_chronology_refusals(void)
{
    static struct test_context context;
    static struct resource_fixture resource_a;
    static struct resource_fixture resource_b;
    static struct claim_fixture claim;
    static struct dcent_receipt_projection projection;
    struct dcent_receipt_storage_head predecessor;
    struct dcent_receipt_storage_head older;
    struct dcent_receipt_storage_head current;
    struct test_row rows[2];

    init_context(&context);
    prepare_attachment(&context, &resource_a, "attach-a");
    append_attachment_status(&context, &resource_a, "pending", 1U, "owner",
                             TX_ID);
    prepare_claim(&context, &claim, BOOT_ID);
    append_claim_status(&context, &claim, "claimed", 2U);
    prepare_attachment(&context, &resource_b, "attach-b");
    append_attachment_status(&context, &resource_b, "pending", 3U, "owner",
                             TX_ID);
    append_claim_status(&context, &claim, "quiescent", 4U);
    memset(&predecessor, 0, sizeof(predecessor));
    predecessor.generation = 2U;
    memset(predecessor.record_sha256, 0x5a,
           sizeof(predecessor.record_sha256));
    rows[0] = (struct test_row){resource_a.kind, resource_a.id,
                                resource_a.intent_hex, 1U,
                                resource_a.status_hex[0]};
    rows[1] = (struct test_row){resource_b.kind, resource_b.id,
                                resource_b.intent_hex, 1U,
                                resource_b.status_hex[0]};
    older = make_head(&context, 3U, &predecessor, "reconciler", CLAIM_ID,
                      "active", 0U, "-", true, CLAIM_ID, claim.intent_hex,
                      1U, claim.status_hex[0], rows, 2U);
    current = make_head(&context, 4U, &older, "reconciler", CLAIM_ID,
                        "active", 0U, "-", true, CLAIM_ID, claim.intent_hex,
                        2U, claim.status_hex[1], rows, 2U);
    init_projection(&projection, &context, &older, &current);
    require(ingest_resource(&projection, &resource_b, NULL) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "post-claim owner resource parses independently");
    require(ingest_claim(&projection, &claim) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "claim directory parses before owner chronology check");
    require(ingest_resource(&projection, &resource_a, NULL) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "pre-claim owner resource parses after claim directory");
    require(dcent_receipt_projection_phase_finish(&projection) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "owner-after-claim phase chain finishes");
    require_finalize_failure_atomic(
        &projection, context.aggregate_bytes,
        DCENT_RECEIPT_PROJECTION_CHRONOLOGY,
        "owner cannot create a resource after reconciliation claim transfer");

    init_context(&context);
    prepare_attachment(&context, &resource_a, "attach-a");
    append_attachment_status(&context, &resource_a, "pending", 1U, "owner",
                             TX_ID);
    append_attachment_status(&context, &resource_a, "active", 2U, "owner",
                             TX_ID);
    prepare_claim(&context, &claim, BOOT_ID);
    append_claim_status(&context, &claim, "claimed", 3U);
    append_claim_status(&context, &claim, "quiescent", 4U);
    append_attachment_status(&context, &resource_a, "release-pending", 5U,
                             "reconciler", CLAIM_ID);
    append_claim_status(&context, &claim, "reconciling", 6U);
    memset(&predecessor, 0, sizeof(predecessor));
    predecessor.generation = 4U;
    memset(predecessor.record_sha256, 0x5a,
           sizeof(predecessor.record_sha256));
    rows[0] = (struct test_row){resource_a.kind, resource_a.id,
                                resource_a.intent_hex, 3U,
                                resource_a.status_hex[2]};
    older = make_head(&context, 5U, &predecessor, "reconciler", CLAIM_ID,
                      "active", 0U, "-", true, CLAIM_ID, claim.intent_hex,
                      2U, claim.status_hex[1], rows, 1U);
    current = make_head(&context, 6U, &older, "reconciler", CLAIM_ID,
                        "active", 0U, "-", true, CLAIM_ID, claim.intent_hex,
                        3U, claim.status_hex[2], rows, 1U);
    init_projection(&projection, &context, &older, &current);
    require(ingest_claim(&projection, &claim) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "claim-first early-reconciler scenario ingests");
    require(ingest_resource(&projection, &resource_a, NULL) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "resource-second early-reconciler scenario ingests");
    require(dcent_receipt_projection_phase_finish(&projection) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "early-reconciler phase chain finishes");
    require_finalize_failure_atomic(
        &projection, context.aggregate_bytes,
        DCENT_RECEIPT_PROJECTION_CHRONOLOGY,
        "reconciler cannot mutate resources before claim revision three");

    init_context(&context);
    prepare_attachment(&context, &resource_a, "attach-a");
    append_attachment_status(&context, &resource_a, "pending", 1U, "owner",
                             TX_ID);
    append_attachment_status(&context, &resource_a, "active", 2U, "owner",
                             TX_ID);
    prepare_claim(&context, &claim, BOOT_ID);
    append_claim_status(&context, &claim, "claimed", 3U);
    append_claim_status(&context, &claim, "quiescent", 4U);
    append_claim_status(&context, &claim, "reconciling", 5U);
    append_claim_status(&context, &claim, "complete", 6U);
    memset(&predecessor, 0, sizeof(predecessor));
    predecessor.generation = 4U;
    memset(predecessor.record_sha256, 0x5a,
           sizeof(predecessor.record_sha256));
    rows[0] = (struct test_row){resource_a.kind, resource_a.id,
                                resource_a.intent_hex, 2U,
                                resource_a.status_hex[1]};
    older = make_head(&context, 5U, &predecessor, "reconciler", CLAIM_ID,
                      "active", 0U, "-", true, CLAIM_ID, claim.intent_hex,
                      3U, claim.status_hex[2], rows, 1U);
    current = make_head(&context, 6U, &older, "reconciler", CLAIM_ID,
                        "active", 0U, "-", true, CLAIM_ID, claim.intent_hex,
                        4U, claim.status_hex[3], rows, 1U);
    init_projection(&projection, &context, &older, &current);
    require(ingest_resource(&projection, &resource_a, NULL) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "early-complete resource history ingests");
    require(ingest_claim(&projection, &claim) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "early-complete claim history ingests");
    require(dcent_receipt_projection_phase_finish(&projection) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "early-complete phase chain finishes");
    require_finalize_failure_atomic(
        &projection, context.aggregate_bytes,
        DCENT_RECEIPT_PROJECTION_CHRONOLOGY,
        "claim completion requires cleanup phase and released resources");

    init_context(&context);
    prepare_attachment(&context, &resource_a, "attach-a");
    append_attachment_status(&context, &resource_a, "pending", 1U, "owner",
                             TX_ID);
    append_attachment_status(&context, &resource_a, "active", 2U, "owner",
                             TX_ID);
    prepare_claim(&context, &claim, BOOT_ID);
    append_claim_status(&context, &claim, "claimed", 3U);
    append_claim_status(&context, &claim, "quiescent", 4U);
    append_claim_status(&context, &claim, "blocked", 5U);
    append_attachment_status(&context, &resource_a, "release-pending", 6U,
                             "reconciler", CLAIM_ID);
    memset(&predecessor, 0, sizeof(predecessor));
    predecessor.generation = 4U;
    memset(predecessor.record_sha256, 0x5a,
           sizeof(predecessor.record_sha256));
    rows[0] = (struct test_row){resource_a.kind, resource_a.id,
                                resource_a.intent_hex, 2U,
                                resource_a.status_hex[1]};
    older = make_head(&context, 5U, &predecessor, "reconciler", CLAIM_ID,
                      "active", 0U, "-", true, CLAIM_ID, claim.intent_hex,
                      3U, claim.status_hex[2], rows, 1U);
    rows[0].revision = 3U;
    rows[0].status = resource_a.status_hex[2];
    current = make_head(&context, 6U, &older, "reconciler", CLAIM_ID,
                        "active", 0U, "-", true, CLAIM_ID, claim.intent_hex,
                        3U, claim.status_hex[2], rows, 1U);
    init_projection(&projection, &context, &older, &current);
    require(ingest_claim(&projection, &claim) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "blocked claim directory parses before resource directory");
    require(ingest_resource(&projection, &resource_a, NULL) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "post-block resource event parses independently");
    require(dcent_receipt_projection_phase_finish(&projection) ==
                DCENT_RECEIPT_PROJECTION_OK,
            "blocked-terminal phase chain finishes");
    require_finalize_failure_atomic(
        &projection, context.aggregate_bytes,
        DCENT_RECEIPT_PROJECTION_CHRONOLOGY,
        "blocked claim is terminal before any later resource event");
}

int main(void)
{
    test_genesis_and_api_hardening();
    test_resource_projection_and_manifest_mismatch();
    test_environment_commit_requires_released_prefix();
    test_claim_boot_binding_and_generation_guards();
    test_valid_takeover_cleanup_and_completion();
    test_cross_chain_chronology_refusals();
    printf("dcentos-receipt projection tests: %u assertions, state=%zu bytes\n",
           assertions, sizeof(struct dcent_receipt_projection));
    return 0;
}
