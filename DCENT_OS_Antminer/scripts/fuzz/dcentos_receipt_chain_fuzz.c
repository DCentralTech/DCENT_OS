/* SPDX-License-Identifier: GPL-3.0-or-later */
#include "receipt_format.h"

#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

static int canonical_seed(const uint8_t *data, size_t size)
{
    static const uint8_t seed[] = "valid\n";

    return size == sizeof(seed) - 1U &&
           memcmp(data, seed, sizeof(seed) - 1U) == 0;
}

static uint8_t phase_default(size_t index)
{
    size_t record = (index - 109U) / 9U;
    size_t field = (index - 109U) % 9U;

    switch (field) {
    case 0U:
        return (record & 1U) == 0U ? DCENT_RECEIPT_LOCK_ENV_COMMIT_ARMED
                                   : DCENT_RECEIPT_LOCK_ACTIVE;
    case 1U:
        return (uint8_t)(record + 1U);
    case 2U:
        return (uint8_t)(record + 9U);
    case 3U:
        return record == 0U ? 0U : 1U;
    case 4U:
        return record == 0U ? 0U : (uint8_t)(0x70U + record - 1U);
    case 5U:
        return DCENT_RECEIPT_ACTOR_OWNER;
    case 6U:
        return 0U;
    case 7U:
        return (uint8_t)(0x70U + record);
    default:
        return 0x11U;
    }
}

static uint8_t input_byte(const uint8_t *data, size_t size, size_t index)
{
    static const uint8_t valid_default[109] = {
        /* Resource intent. */
        1, 0, 0, 1, 0x11, 0x22,
        /* pending */
        1, 0, 0, 0x11, 0x22, 1, 1, 0, 0, 1, 0, 0x31, 1,
        /* active */
        1, 0, 0, 0x11, 0x22, 2, 2, 1, 0x31, 1, 0, 0x32, 2,
        /* release-pending */
        1, 0, 0, 0x11, 0x22, 3, 3, 1, 0x32, 1, 0, 0x33, 3,
        /* released */
        1, 0, 0, 0x11, 0x22, 4, 4, 1, 0x33, 1, 0, 0x34, 4,
        /* Claim intent. */
        0, 0, 0, 1, 0x11, 0x41,
        /* claimed */
        0x41, 1, 1, 0, 0, 0, 0, 0, 0x51, 5,
        /* quiescent */
        0x41, 2, 2, 1, 0x51, 0, 1, 0x61, 0x52, 6,
        /* reconciling */
        0x41, 3, 3, 1, 0x52, 0, 1, 0x61, 0x53, 7,
        /* complete */
        0x41, 4, 4, 1, 0x53, 0, 1, 0x61, 0x54, 8,
        /* Summary: one resource, claim present, bounded bytes. */
        1, 1, 1,
        /* Binding: matching transaction and digest. */
        0, 0x11,
    };

    if (!canonical_seed(data, size) && index < size)
        return data[index];
    if (index < sizeof(valid_default))
        return valid_default[index];
    if (index < 109U + DCENT_RECEIPT_MAX_PHASE_REVISIONS * 9U)
        return phase_default(index);
    return 0U;
}

static struct dcent_receipt_slice selected_id(uint8_t selector)
{
    static const unsigned char first[] = "Tx-1";
    static const unsigned char second[] = "Other-2";
    struct dcent_receipt_slice value;

    value.data = (selector & 1U) == 0U ? first : second;
    value.size = (selector & 1U) == 0U ? sizeof(first) - 1U
                                      : sizeof(second) - 1U;
    return value;
}

static struct dcent_receipt_slice literal_slice(const char *literal)
{
    struct dcent_receipt_slice value;

    value.data = (const unsigned char *)literal;
    value.size = strlen(literal);
    return value;
}

static void fill_digest(unsigned char *digest, uint8_t value)
{
    memset(digest, value, DCENT_RECEIPT_SHA256_BYTES);
}

int LLVMFuzzerTestOneInput(const uint8_t *data, size_t size)
{
    struct dcent_receipt_resource_intent resource_intent;
    struct dcent_receipt_resource_status resource_status;
    struct dcent_receipt_resource_chain resources[2];
    struct dcent_receipt_claim_intent claim_intent;
    struct dcent_receipt_claim_status claim_status;
    struct dcent_receipt_claim_chain claim;
    struct dcent_receipt_transaction_phase_status phase_status;
    struct dcent_receipt_transaction_phase_chain phase;
    struct dcent_receipt_resource_chain resource_snapshot;
    struct dcent_receipt_claim_chain claim_snapshot;
    struct dcent_receipt_transaction_phase_chain phase_snapshot;
    struct dcent_receipt_binding binding;
    struct dcent_receipt_binding_anchor anchor;
    size_t index;
    int result;
    int require_valid = canonical_seed(data, size);

    memset(&binding, 0, sizeof(binding));
    binding.transaction_id = selected_id(input_byte(data, size, 107U));
    binding.boot_id =
        literal_slice("abcdef12-3456-7890-abcd-ef1234567890");
    binding.owner_pid = 123U;
    binding.owner_starttime = 456U;
    binding.owner_mount_namespace.device = 1U;
    binding.owner_mount_namespace.inode = 22U;
    binding.acquisition_guard_device_inode.device = 9U;
    binding.acquisition_guard_device_inode.inode = 10U;
    binding.transaction_lock_path =
        literal_slice("/run/dcentos-sysupgrade.lock");
    binding.transaction_lock_device_inode.device = 2U;
    binding.transaction_lock_device_inode.inode = 33U;
    binding.transaction_lock_owner_device_inode.device = 2U;
    binding.transaction_lock_owner_device_inode.inode = 34U;
    binding.transaction_lock_owner_sha256.present = true;
    fill_digest(binding.transaction_lock_owner_sha256.bytes, 0x42U);
    binding.storage_mount_id = 7U;
    binding.ledger_path =
        literal_slice("/run/dcentos-sysupgrade.lock/ledger");
    binding.ledger_device_inode.device = 5U;
    binding.ledger_device_inode.inode = 99U;
    fill_digest(binding.record_sha256, input_byte(data, size, 108U));
    memset(&anchor, 0, sizeof(anchor));
    if (dcent_receipt_binding_anchor_init(&anchor, &binding) !=
        DCENT_RECEIPT_FORMAT_OK)
        abort();

    memset(&resource_intent, 0, sizeof(resource_intent));
    resource_intent.kind = (enum dcent_receipt_resource_kind)(
        input_byte(data, size, 0) % 6U);
    resource_intent.transaction_id = selected_id(input_byte(data, size, 1));
    resource_intent.resource_id = selected_id(input_byte(data, size, 2));
    resource_intent.binding_sha256.present =
        (input_byte(data, size, 3) & 1U) != 0U;
    fill_digest(resource_intent.binding_sha256.bytes,
                input_byte(data, size, 4));
    fill_digest(resource_intent.record_sha256, input_byte(data, size, 5));
    memset(resources, 0, sizeof(resources));
    resource_snapshot = resources[0];
    result = dcent_receipt_resource_chain_begin(&resources[0], &anchor,
                                                &resource_intent);
    if (result != DCENT_RECEIPT_FORMAT_OK &&
        memcmp(&resources[0], &resource_snapshot, sizeof(resource_snapshot)) !=
            0)
        abort();

    for (index = 0; index < DCENT_RECEIPT_MAX_REVISIONS; ++index) {
        size_t base = 6U + index * 13U;

        memset(&resource_status, 0, sizeof(resource_status));
        resource_status.kind = (enum dcent_receipt_resource_kind)(
            input_byte(data, size, base) % 6U);
        resource_status.transaction_id =
            selected_id(input_byte(data, size, base + 1U));
        resource_status.resource_id =
            selected_id(input_byte(data, size, base + 2U));
        resource_status.binding_sha256.present = true;
        fill_digest(resource_status.binding_sha256.bytes,
                    input_byte(data, size, base + 3U));
        resource_status.intent_sha256.present = true;
        fill_digest(resource_status.intent_sha256.bytes,
                    input_byte(data, size, base + 4U));
        resource_status.phase = (enum dcent_receipt_resource_phase)(
            input_byte(data, size, base + 5U) % 7U);
        resource_status.revision = input_byte(data, size, base + 6U) % 6U;
        resource_status.ledger_generation =
            input_byte(data, size, base + 12U);
        resource_status.previous_status_sha256.present =
            (input_byte(data, size, base + 7U) & 1U) != 0U;
        fill_digest(resource_status.previous_status_sha256.bytes,
                    input_byte(data, size, base + 8U));
        resource_status.actor_kind = (enum dcent_receipt_actor_kind)(
            input_byte(data, size, base + 9U) % 4U);
        resource_status.actor_id =
            selected_id(input_byte(data, size, base + 10U));
        fill_digest(resource_status.record_sha256,
                    input_byte(data, size, base + 11U));
        resource_snapshot = resources[0];
        result =
            dcent_receipt_resource_chain_add(&resources[0], &resource_status);
        if (result != DCENT_RECEIPT_FORMAT_OK &&
            memcmp(&resources[0], &resource_snapshot,
                   sizeof(resource_snapshot)) != 0)
            abort();
        if (result == DCENT_RECEIPT_FORMAT_OK &&
            (resources[0].revisions != resource_status.revision ||
             resources[0].latest_ledger_generation !=
                 resource_status.ledger_generation ||
             resources[0].latest_phase != resource_status.phase ||
             (resource_status.actor_kind == DCENT_RECEIPT_ACTOR_RECONCILER &&
              resources[0].authority !=
                  DCENT_RECEIPT_AUTHORITY_RECONCILER)))
            abort();
    }
    result = dcent_receipt_resource_chain_finish(&resources[0]);
    if (require_valid && result != DCENT_RECEIPT_FORMAT_OK)
        abort();

    memset(&claim_intent, 0, sizeof(claim_intent));
    claim_intent.transaction_id = selected_id(input_byte(data, size, 59U));
    claim_intent.claim_id = selected_id(input_byte(data, size, 60U));
    claim_intent.binding_sha256.present =
        (input_byte(data, size, 61U) & 1U) != 0U;
    fill_digest(claim_intent.binding_sha256.bytes,
                input_byte(data, size, 62U));
    fill_digest(claim_intent.record_sha256, input_byte(data, size, 63U));
    claim_intent.reconciler_boot_id =
        literal_slice("abcdef12-3456-7890-abcd-ef1234567890");
    claim_intent.reconciler_pid = 321U;
    claim_intent.reconciler_starttime = 987654U;
    claim_intent.reconciler_mount_namespace.device = 3U;
    claim_intent.reconciler_mount_namespace.inode = 77U;
    claim_intent.maintenance_lock_path =
        literal_slice("/run/dcentos-maintenance.lock");
    claim_intent.maintenance_lock_device_inode.device = 4U;
    claim_intent.maintenance_lock_device_inode.inode = 88U;
    claim_intent.evidence.type = DCENT_RECEIPT_EVIDENCE_OWNER_DEATH;
    claim_intent.evidence.digest.present = true;
    fill_digest(claim_intent.evidence.digest.bytes, 0x71U);
    memset(&claim, 0, sizeof(claim));
    claim_snapshot = claim;
    result = dcent_receipt_claim_chain_begin(&claim, &anchor, &claim_intent);
    if (result != DCENT_RECEIPT_FORMAT_OK &&
        memcmp(&claim, &claim_snapshot, sizeof(claim_snapshot)) != 0)
        abort();

    for (index = 0; index < DCENT_RECEIPT_MAX_REVISIONS; ++index) {
        size_t base = 64U + index * 10U;

        memset(&claim_status, 0, sizeof(claim_status));
        claim_status.claim_intent_sha256.present = true;
        fill_digest(claim_status.claim_intent_sha256.bytes,
                    input_byte(data, size, base));
        claim_status.phase = (enum dcent_receipt_claim_phase)(
            input_byte(data, size, base + 1U) % 7U);
        claim_status.revision = input_byte(data, size, base + 2U) % 6U;
        claim_status.ledger_generation =
            input_byte(data, size, base + 9U);
        claim_status.previous_status_sha256.present =
            (input_byte(data, size, base + 3U) & 1U) != 0U;
        fill_digest(claim_status.previous_status_sha256.bytes,
                    input_byte(data, size, base + 4U));
        claim_status.actor_id =
            selected_id(input_byte(data, size, base + 5U));
        claim_status.quiescence_sha256.present =
            (input_byte(data, size, base + 6U) & 1U) != 0U;
        fill_digest(claim_status.quiescence_sha256.bytes,
                    input_byte(data, size, base + 7U));
        fill_digest(claim_status.record_sha256,
                    input_byte(data, size, base + 8U));
        claim_snapshot = claim;
        result = dcent_receipt_claim_chain_add(&claim, &claim_status);
        if (result != DCENT_RECEIPT_FORMAT_OK &&
            memcmp(&claim, &claim_snapshot, sizeof(claim_snapshot)) != 0)
            abort();
        if (result == DCENT_RECEIPT_FORMAT_OK &&
            (claim.revisions != claim_status.revision ||
             claim.latest_ledger_generation !=
                 claim_status.ledger_generation ||
             claim.latest_phase != claim_status.phase ||
             (claim_status.phase == DCENT_RECEIPT_CLAIM_RECONCILING &&
              !claim.saw_reconciling)))
            abort();
    }
    result = dcent_receipt_claim_chain_finish(&claim);
    if (require_valid && result != DCENT_RECEIPT_FORMAT_OK)
        abort();

    memset(&phase, 0, sizeof(phase));
    phase_snapshot = phase;
    result = dcent_receipt_transaction_phase_chain_begin(&phase, &anchor);
    if (result != DCENT_RECEIPT_FORMAT_OK &&
        memcmp(&phase, &phase_snapshot, sizeof(phase_snapshot)) != 0)
        abort();
    if (require_valid && result != DCENT_RECEIPT_FORMAT_OK)
        abort();

    for (index = 0; index < DCENT_RECEIPT_MAX_PHASE_REVISIONS; ++index) {
        size_t base = 109U + index * 9U;

        memset(&phase_status, 0, sizeof(phase_status));
        phase_status.binding_sha256.present = true;
        fill_digest(phase_status.binding_sha256.bytes,
                    input_byte(data, size, base + 8U));
        phase_status.transaction_id =
            selected_id(input_byte(data, size, base + 6U));
        phase_status.phase = (enum dcent_receipt_lock_phase)(
            input_byte(data, size, base) % 5U);
        phase_status.revision = input_byte(data, size, base + 1U) % 18U;
        phase_status.ledger_generation =
            input_byte(data, size, base + 2U);
        phase_status.previous_status_sha256.present =
            (input_byte(data, size, base + 3U) & 1U) != 0U;
        fill_digest(phase_status.previous_status_sha256.bytes,
                    input_byte(data, size, base + 4U));
        phase_status.actor_kind = (enum dcent_receipt_actor_kind)(
            input_byte(data, size, base + 5U) % 4U);
        phase_status.actor_id =
            selected_id(input_byte(data, size, base + 6U));
        fill_digest(phase_status.record_sha256,
                    input_byte(data, size, base + 7U));
        phase_snapshot = phase;
        result = dcent_receipt_transaction_phase_chain_add(&phase,
                                                           &phase_status);
        if (result != DCENT_RECEIPT_FORMAT_OK &&
            memcmp(&phase, &phase_snapshot, sizeof(phase_snapshot)) != 0)
            abort();
        if (result == DCENT_RECEIPT_FORMAT_OK &&
            (phase.revisions != phase_status.revision ||
             phase.latest_ledger_generation !=
                 phase_status.ledger_generation ||
             phase.latest_phase != phase_status.phase))
            abort();
    }
    result = dcent_receipt_transaction_phase_chain_finish(&phase);
    if (require_valid && result != DCENT_RECEIPT_FORMAT_OK)
        abort();

    resources[1] = resources[0];
    result = dcent_receipt_ledger_validate_summary(
        &anchor, resources, input_byte(data, size, 104U) % 3U,
        (input_byte(data, size, 105U) & 1U) != 0U ? &claim : NULL,
        (size_t)input_byte(data, size, 106U) * 4096U);
    if (require_valid && result != DCENT_RECEIPT_FORMAT_OK)
        abort();
    return 0;
}
