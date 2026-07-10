import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { api } from '../api/client';
import type { DeviceCapabilityDescriptor } from '../api/generated/capability';
import {
  platformGateFromDeviceCapability,
  type PlatformGate,
} from '../utils/platformCapabilities';

export interface DeviceCapabilityState extends PlatformGate {
  descriptor: DeviceCapabilityDescriptor | null;
  loading: boolean;
  error: string | null;
  refresh: () => Promise<void>;
}

function errorMessage(error: unknown): string {
  if (error instanceof Error && error.message.trim()) {
    return error.message;
  }
  return 'failed to load device capability descriptor';
}

export function useDeviceCapability(fallbackPlatformKey?: string): DeviceCapabilityState {
  const mounted = useRef(true);
  const [descriptor, setDescriptor] = useState<DeviceCapabilityDescriptor | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    return () => {
      mounted.current = false;
    };
  }, []);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const next = await api.getDeviceCapability();
      if (!mounted.current) return;
      setDescriptor(next);
      setError(null);
    } catch (err) {
      if (!mounted.current) return;
      setDescriptor(null);
      setError(errorMessage(err));
    } finally {
      if (mounted.current) {
        setLoading(false);
      }
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const gate = useMemo(
    () => platformGateFromDeviceCapability(descriptor, fallbackPlatformKey),
    [descriptor, fallbackPlatformKey],
  );

  return {
    ...gate,
    descriptor,
    loading,
    error,
    refresh,
  };
}
