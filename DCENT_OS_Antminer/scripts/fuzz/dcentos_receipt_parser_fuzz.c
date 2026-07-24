/* SPDX-License-Identifier: GPL-3.0-or-later */
#include "receipt_format.h"

#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

int LLVMFuzzerTestOneInput(const uint8_t *data, size_t size)
{
    struct dcent_receipt_binding binding;
    struct dcent_receipt_resource_intent resource_intent;
    struct dcent_receipt_resource_status resource_status;
    struct dcent_receipt_claim_intent claim_intent;
    struct dcent_receipt_claim_status claim_status;
    struct dcent_receipt_lock_owner lock_owner;
    struct dcent_receipt_transaction_phase_status phase_status;
    struct dcent_receipt_binding binding_sentinel;
    struct dcent_receipt_resource_intent resource_intent_sentinel;
    struct dcent_receipt_resource_status resource_status_sentinel;
    struct dcent_receipt_claim_intent claim_intent_sentinel;
    struct dcent_receipt_claim_status claim_status_sentinel;
    struct dcent_receipt_lock_owner lock_owner_sentinel;
    struct dcent_receipt_transaction_phase_status phase_status_sentinel;
    int result;

    memset(&binding_sentinel, 0xa5, sizeof(binding_sentinel));
    binding = binding_sentinel;
    result = dcent_receipt_parse_binding_abi1(data, size, &binding);
    if (result != DCENT_RECEIPT_FORMAT_OK &&
        memcmp(&binding, &binding_sentinel, sizeof(binding)) != 0)
        abort();

    memset(&lock_owner_sentinel, 0xa5, sizeof(lock_owner_sentinel));
    lock_owner = lock_owner_sentinel;
    result = dcent_receipt_parse_lock_owner_v3(data, size, &lock_owner);
    if (result != DCENT_RECEIPT_FORMAT_OK &&
        memcmp(&lock_owner, &lock_owner_sentinel, sizeof(lock_owner)) != 0)
        abort();

    memset(&resource_intent_sentinel, 0xa5,
           sizeof(resource_intent_sentinel));
    resource_intent = resource_intent_sentinel;
    result = dcent_receipt_parse_resource_intent_abi1(data, size,
                                                       &resource_intent);
    if (result != DCENT_RECEIPT_FORMAT_OK &&
        memcmp(&resource_intent, &resource_intent_sentinel,
               sizeof(resource_intent)) != 0)
        abort();

    memset(&resource_status_sentinel, 0xa5,
           sizeof(resource_status_sentinel));
    resource_status = resource_status_sentinel;
    result = dcent_receipt_parse_resource_status_abi1(data, size,
                                                       &resource_status);
    if (result != DCENT_RECEIPT_FORMAT_OK &&
        memcmp(&resource_status, &resource_status_sentinel,
               sizeof(resource_status)) != 0)
        abort();

    memset(&claim_intent_sentinel, 0xa5, sizeof(claim_intent_sentinel));
    claim_intent = claim_intent_sentinel;
    result = dcent_receipt_parse_claim_intent_abi1(data, size, &claim_intent);
    if (result != DCENT_RECEIPT_FORMAT_OK &&
        memcmp(&claim_intent, &claim_intent_sentinel,
               sizeof(claim_intent)) != 0)
        abort();

    memset(&claim_status_sentinel, 0xa5, sizeof(claim_status_sentinel));
    claim_status = claim_status_sentinel;
    result = dcent_receipt_parse_claim_status_abi1(data, size, &claim_status);
    if (result != DCENT_RECEIPT_FORMAT_OK &&
        memcmp(&claim_status, &claim_status_sentinel,
               sizeof(claim_status)) != 0)
        abort();

    memset(&phase_status_sentinel, 0xa5, sizeof(phase_status_sentinel));
    phase_status = phase_status_sentinel;
    result = dcent_receipt_parse_transaction_phase_status_abi2(
        data, size, &phase_status);
    if (result != DCENT_RECEIPT_FORMAT_OK &&
        memcmp(&phase_status, &phase_status_sentinel,
               sizeof(phase_status)) != 0)
        abort();
    return 0;
}
