import { useEffect, useState } from 'react';
import { useMinerStore } from '../store/miner';
import { getDashboardHealth } from '../utils/health';

export function useDashboardHealth() {
  const status = useMinerStore(s => s.status);
  const wsConnected = useMinerStore(s => s.wsConnected);
  const lastUpdate = useMinerStore(s => s.lastUpdate);
  const setupStatus = useMinerStore(s => s.setupStatus);
  const [tick, setTick] = useState(0);

  useEffect(() => {
    const timer = setInterval(() => setTick(prev => prev + 1), 5000);
    return () => clearInterval(timer);
  }, []);

  void tick;

  return getDashboardHealth({ status, wsConnected, lastUpdate, setupStatus });
}
