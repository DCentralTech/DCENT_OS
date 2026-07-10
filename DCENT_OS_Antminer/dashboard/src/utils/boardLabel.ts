// Canonical hashboard label helper (Omega P3-5).
//
// The dashboard historically mixed three vocabularies for the SAME physical
// hashboard: the FPGA connector silk label ("J6"/"J7"/"J8"), the daemon chain
// id ("Chain 6" / "CH0"), and a bare position. This helper produces ONE
// canonical label so an operator never has to reconcile them:
//
//     Board 1 (J6 · chain6)
//
//   - "Board N"  — 1-based physical board position (what the operator counts
//                  top-to-bottom in the chassis).
//   - "Jx"       — the FPGA connector silk-screen label. On the Zynq S9/S17/S19
//                  control board the chains land on connectors J6/J7/J8
//                  (position 0→J6, 1→J7, 2→J8). This is POSITION-based, not
//                  chain-id-based, so a unit whose populated chains are e.g.
//                  ids 0 and 2 still reads J6/J7 by slot. Beyond the known
//                  connectors we fall back to `J<chainId>` (preserves the prior
//                  per-strip `CONNECTOR_LABELS[i] ?? \`J${chain.id}\`` behavior).
//   - "chainN"   — the daemon's chain id (status.chains[].id), the value every
//                  REST/log/CGMiner surface keys on.
//
// Pure + dependency-free so it is unit-testable and reusable across strips.

// FPGA connector silk labels in physical slot order (Zynq S9/S17/S19 control
// board). Index = board position, NOT chain id.
const CONNECTOR_LABELS = ['J6', 'J7', 'J8', 'J9'] as const;

/** "J6" — the connector silk label for a board position; falls back to
 *  `J<chainId>` past the known connectors. */
export function boardConnector(index: number, chainId: number): string {
  return CONNECTOR_LABELS[index] ?? `J${chainId}`;
}

/** "Board 1" — the 1-based physical board position. */
export function boardName(index: number): string {
  return `Board ${index + 1}`;
}

/** "J6 · chain6" — the connector + chain-id descriptor (no "Board N" prefix),
 *  for surfaces that already render the board name in a separate element. */
export function boardDescriptor(index: number, chainId: number): string {
  return `${boardConnector(index, chainId)} · chain${chainId}`;
}

/** "Board 1 (J6 · chain6)" — the full canonical one-line label. */
export function boardLabel(index: number, chainId: number): string {
  return `${boardName(index)} (${boardDescriptor(index, chainId)})`;
}
