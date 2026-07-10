import { useEffect } from 'react';
import { useDashboardHealth } from '../hooks/useDashboardHealth';
import { useMinerStore } from '../store/miner';
import { readFxSettings } from './fxSettings';
import { updateTitleTicker } from './titleTicker';

export function useTitleTicker(): void {
  const status = useMinerStore(s => s.status);
  const health = useDashboardHealth();

  useEffect(() => {
    updateTitleTicker({
      hashrateGhs: status?.hashrate_ghs ?? 0,
      hasRecentTelemetry: health.hasRecentTelemetry,
      enabled: readFxSettings().titleTicker,
    });
  }, [health.hasRecentTelemetry, status?.hashrate_ghs]);
}
