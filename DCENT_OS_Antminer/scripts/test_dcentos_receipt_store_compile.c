/* SPDX-License-Identifier: GPL-3.0-or-later */
#include "receipt_store.h"

#include <string.h>

int main(void)
{
    struct dcent_receipt_ledger_snapshot snapshot;
    struct dcent_receipt_store_error error;

    memset(&snapshot, 0, sizeof(snapshot));
    return dcent_receipt_store_result_name(DCENT_RECEIPT_STORE_RACE) != NULL &&
                   dcent_receipt_store_scan_forensic_abi1(
                       -1, &snapshot, &error) ==
                       DCENT_RECEIPT_STORE_INVALID_ARGUMENT
               ? 0
               : 1;
}
