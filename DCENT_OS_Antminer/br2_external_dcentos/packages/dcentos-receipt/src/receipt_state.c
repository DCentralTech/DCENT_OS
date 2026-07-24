/* SPDX-License-Identifier: GPL-3.0-or-later */
#include "receipt_state.h"

#include <stddef.h>
#include <string.h>

struct name_entry {
    int value;
    const char *name;
};

static int parse_name(const struct name_entry *entries, size_t count,
                      const char *text)
{
    size_t index;

    if (text == NULL)
        return 0;
    for (index = 0; index < count; ++index) {
        if (strcmp(entries[index].name, text) == 0)
            return entries[index].value;
    }
    return 0;
}

static const char *lookup_name(const struct name_entry *entries, size_t count,
                               int value)
{
    size_t index;

    for (index = 0; index < count; ++index) {
        if (entries[index].value == value)
            return entries[index].name;
    }
    return NULL;
}

static const struct name_entry provenance_names[] = {
    {DCENT_RECEIPT_PROVENANCE_CREATED, "created"},
    {DCENT_RECEIPT_PROVENANCE_BORROWED, "borrowed"},
};

static const struct name_entry resource_phase_names[] = {
    {DCENT_RECEIPT_RESOURCE_PENDING, "pending"},
    {DCENT_RECEIPT_RESOURCE_ACTIVE, "active"},
    {DCENT_RECEIPT_RESOURCE_RELEASE_PENDING, "release-pending"},
    {DCENT_RECEIPT_RESOURCE_RELEASED, "released"},
    {DCENT_RECEIPT_RESOURCE_CONFLICT, "conflict"},
};

static const struct name_entry claim_phase_names[] = {
    {DCENT_RECEIPT_CLAIM_CLAIMED, "claimed"},
    {DCENT_RECEIPT_CLAIM_QUIESCENT, "quiescent"},
    {DCENT_RECEIPT_CLAIM_RECONCILING, "reconciling"},
    {DCENT_RECEIPT_CLAIM_COMPLETE, "complete"},
    {DCENT_RECEIPT_CLAIM_BLOCKED, "blocked"},
};

enum dcent_receipt_provenance dcent_receipt_provenance_parse(const char *text)
{
    return (enum dcent_receipt_provenance)parse_name(
        provenance_names,
        sizeof(provenance_names) / sizeof(provenance_names[0]), text);
}

const char *dcent_receipt_provenance_name(enum dcent_receipt_provenance value)
{
    return lookup_name(provenance_names,
                       sizeof(provenance_names) / sizeof(provenance_names[0]),
                       value);
}

enum dcent_receipt_resource_phase dcent_receipt_resource_phase_parse(
    const char *text)
{
    return (enum dcent_receipt_resource_phase)parse_name(
        resource_phase_names,
        sizeof(resource_phase_names) / sizeof(resource_phase_names[0]), text);
}

const char *dcent_receipt_resource_phase_name(
    enum dcent_receipt_resource_phase value)
{
    return lookup_name(
        resource_phase_names,
        sizeof(resource_phase_names) / sizeof(resource_phase_names[0]), value);
}

bool dcent_receipt_resource_transition_valid(
    enum dcent_receipt_resource_phase from,
    enum dcent_receipt_resource_phase to)
{
    switch (from) {
    case DCENT_RECEIPT_RESOURCE_PENDING:
        return to == DCENT_RECEIPT_RESOURCE_ACTIVE ||
               to == DCENT_RECEIPT_RESOURCE_RELEASED ||
               to == DCENT_RECEIPT_RESOURCE_CONFLICT;
    case DCENT_RECEIPT_RESOURCE_ACTIVE:
        return to == DCENT_RECEIPT_RESOURCE_RELEASE_PENDING ||
               to == DCENT_RECEIPT_RESOURCE_CONFLICT;
    case DCENT_RECEIPT_RESOURCE_RELEASE_PENDING:
        return to == DCENT_RECEIPT_RESOURCE_RELEASED ||
               to == DCENT_RECEIPT_RESOURCE_CONFLICT;
    case DCENT_RECEIPT_RESOURCE_INVALID:
    case DCENT_RECEIPT_RESOURCE_RELEASED:
    case DCENT_RECEIPT_RESOURCE_CONFLICT:
        return false;
    }
    return false;
}

bool dcent_receipt_resource_phase_terminal(
    enum dcent_receipt_resource_phase phase)
{
    return phase == DCENT_RECEIPT_RESOURCE_RELEASED ||
           phase == DCENT_RECEIPT_RESOURCE_CONFLICT;
}

enum dcent_receipt_claim_phase dcent_receipt_claim_phase_parse(
    const char *text)
{
    return (enum dcent_receipt_claim_phase)parse_name(
        claim_phase_names,
        sizeof(claim_phase_names) / sizeof(claim_phase_names[0]), text);
}

const char *dcent_receipt_claim_phase_name(enum dcent_receipt_claim_phase value)
{
    return lookup_name(
        claim_phase_names,
        sizeof(claim_phase_names) / sizeof(claim_phase_names[0]), value);
}

bool dcent_receipt_claim_transition_valid(enum dcent_receipt_claim_phase from,
                                          enum dcent_receipt_claim_phase to)
{
    switch (from) {
    case DCENT_RECEIPT_CLAIM_CLAIMED:
        return to == DCENT_RECEIPT_CLAIM_QUIESCENT ||
               to == DCENT_RECEIPT_CLAIM_BLOCKED;
    case DCENT_RECEIPT_CLAIM_QUIESCENT:
        return to == DCENT_RECEIPT_CLAIM_RECONCILING ||
               to == DCENT_RECEIPT_CLAIM_BLOCKED;
    case DCENT_RECEIPT_CLAIM_RECONCILING:
        return to == DCENT_RECEIPT_CLAIM_COMPLETE ||
               to == DCENT_RECEIPT_CLAIM_BLOCKED;
    case DCENT_RECEIPT_CLAIM_INVALID:
    case DCENT_RECEIPT_CLAIM_COMPLETE:
    case DCENT_RECEIPT_CLAIM_BLOCKED:
        return false;
    }
    return false;
}

bool dcent_receipt_claim_phase_terminal(enum dcent_receipt_claim_phase phase)
{
    return phase == DCENT_RECEIPT_CLAIM_COMPLETE ||
           phase == DCENT_RECEIPT_CLAIM_BLOCKED;
}

bool dcent_receipt_release_requires_destroy(
    enum dcent_receipt_provenance provenance)
{
    return provenance == DCENT_RECEIPT_PROVENANCE_CREATED;
}
