/* SPDX-License-Identifier: GPL-3.0-or-later */
#ifndef DCENTOS_RECEIPT_STATE_H
#define DCENTOS_RECEIPT_STATE_H

#include <stdbool.h>

enum dcent_receipt_provenance {
    DCENT_RECEIPT_PROVENANCE_INVALID = 0,
    DCENT_RECEIPT_PROVENANCE_CREATED,
    DCENT_RECEIPT_PROVENANCE_BORROWED,
};

enum dcent_receipt_resource_phase {
    DCENT_RECEIPT_RESOURCE_INVALID = 0,
    DCENT_RECEIPT_RESOURCE_PENDING,
    DCENT_RECEIPT_RESOURCE_ACTIVE,
    DCENT_RECEIPT_RESOURCE_RELEASE_PENDING,
    DCENT_RECEIPT_RESOURCE_RELEASED,
    DCENT_RECEIPT_RESOURCE_CONFLICT,
};

enum dcent_receipt_claim_phase {
    DCENT_RECEIPT_CLAIM_INVALID = 0,
    DCENT_RECEIPT_CLAIM_CLAIMED,
    DCENT_RECEIPT_CLAIM_QUIESCENT,
    DCENT_RECEIPT_CLAIM_RECONCILING,
    DCENT_RECEIPT_CLAIM_COMPLETE,
    DCENT_RECEIPT_CLAIM_BLOCKED,
};

enum dcent_receipt_provenance dcent_receipt_provenance_parse(const char *text);
const char *dcent_receipt_provenance_name(enum dcent_receipt_provenance value);

enum dcent_receipt_resource_phase dcent_receipt_resource_phase_parse(
    const char *text);
const char *dcent_receipt_resource_phase_name(
    enum dcent_receipt_resource_phase value);
bool dcent_receipt_resource_transition_valid(
    enum dcent_receipt_resource_phase from,
    enum dcent_receipt_resource_phase to);
bool dcent_receipt_resource_phase_terminal(
    enum dcent_receipt_resource_phase phase);

enum dcent_receipt_claim_phase dcent_receipt_claim_phase_parse(
    const char *text);
const char *dcent_receipt_claim_phase_name(enum dcent_receipt_claim_phase value);
bool dcent_receipt_claim_transition_valid(enum dcent_receipt_claim_phase from,
                                          enum dcent_receipt_claim_phase to);
bool dcent_receipt_claim_phase_terminal(enum dcent_receipt_claim_phase phase);

bool dcent_receipt_release_requires_destroy(
    enum dcent_receipt_provenance provenance);

#endif
