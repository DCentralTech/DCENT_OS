import { useEffect } from 'react';
import { useDashboardHealth } from './useDashboardHealth';
import { useMinerStore } from '../store/miner';
import { isHealthIssueDismissed } from '../utils/health';

export function useHealthAlertBridge() {
  const health = useDashboardHealth();
  const dataLoaded = useMinerStore(s => s.dataLoaded);
  const lastUpdate = useMinerStore(s => s.lastUpdate);
  const upsertHealthAlert = useMinerStore(s => s.upsertHealthAlert);
  const clearHealthAlert = useMinerStore(s => s.clearHealthAlert);

  useEffect(() => {
    if (!dataLoaded || lastUpdate === 0) {
      return;
    }

    const activeKeys = new Set<string>();

    for (const issue of health.issues) {
      // Freedom-first: a dismissible issue the operator has already
      // dismissed (persisted in localStorage) must NOT be re-upserted —
      // otherwise the "no owner password" reminder would nag forever.
      // It still self-clears below regardless (e.g. when a password is
      // set the issue stops being emitted, so the alert is cleared).
      if (issue.dismissible && isHealthIssueDismissed(issue.key)) {
        continue;
      }
      activeKeys.add(issue.key);
      upsertHealthAlert({
        key: issue.key,
        level: issue.level,
        message: issue.message,
      });
    }

    const knownHealthAlerts = useMinerStore.getState().alerts.filter(alert => alert.source === 'health');
    for (const alert of knownHealthAlerts) {
      if (alert.dedupeKey && !activeKeys.has(alert.dedupeKey)) {
        clearHealthAlert(alert.dedupeKey);
      }
    }
  }, [clearHealthAlert, dataLoaded, health.issues, lastUpdate, upsertHealthAlert]);
}
