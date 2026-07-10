// Temperature unit hook — reads preference from miner store, provides
// a format() helper that converts Celsius to the user's preferred unit.

import { useMinerStore } from '../store/miner';
import { toDisplayTemp, tempUnitSymbol } from '../utils/thermal';

export function useTemp() {
  const unit = useMinerStore(s => s.settings?.temperatureUnit ?? 'C');
  return {
    /** Format a Celsius value as "XX.X°C" or "XX.X°F" depending on user pref. */
    format: (c: number) => `${toDisplayTemp(c, unit).toFixed(1)}${tempUnitSymbol(unit)}`,
    /** Convert Celsius to display unit (numeric). */
    convert: (c: number) => toDisplayTemp(c, unit),
    /** The raw unit symbol string, e.g. "°C" or "°F". */
    symbol: tempUnitSymbol(unit),
    /** The raw unit key: 'C' or 'F'. */
    unit,
  };
}
