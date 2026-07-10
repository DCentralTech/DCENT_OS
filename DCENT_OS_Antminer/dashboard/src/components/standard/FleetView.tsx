import { useEffect, useMemo, useState } from "react";
import {
  getFleetPoolStats,
  identifyLocalMiner,
  identifyRemoteMiner,
  listFleetMiners,
  type FleetMiner,
  type FleetMinerStatus,
  type FleetPoolMinerSnapshot,
  type FleetResponse,
  type FleetPoolStatsResponse,
} from "../../api/fleet";
import { useMinerStore } from "../../store/miner";
import { EmptyState } from "../common/EmptyState";
import { NoFleetIllustration } from "../common/illustrations";
import { PageSkeleton } from "../common/skeletons";
import { Tooltip } from "../common/Tooltip";
import { useWindowedList } from "../../hooks/useWindowedList";

type SortKey =
  | "hostname"
  | "ip"
  | "model"
  | "hashrate_ghs"
  | "temp_c"
  | "fan_pwm"
  | "status"
  | "last_seen_ms"
  | "pool_target_difficulty"
  | "achieved_difficulty"
  | "pool_url"
  | "pool_worker"
  | "acceptance_rate";

type SortDirection = "asc" | "desc";
type SortValue = string | number;
type IdentifyStatus = "idle" | "pending" | "sent" | "fallback" | "failed";

type FleetMinerRow = FleetMiner & {
  pool_url?: string | null;
  pool_worker?: string | null;
  acceptance_rate?: number | null;
};

type IdentifyState = Record<string, {
  status: IdentifyStatus;
  message: string;
  href?: string;
}>;

interface FleetColumn {
  key: SortKey;
  label: string;
  align?: "right";
  sortable?: boolean;
  render: (miner: FleetMinerRow) => string;
  sortValue: (miner: FleetMinerRow) => SortValue;
}

//  truth contract — reuse the exact same threshold that SharesPage uses.
// NEVER change this value without a coordinated memory-rule update +  sign-off.
const LUCKY_THRESHOLD_MULTIPLIER = 4;

function isLuckyShare(achieved?: number | null, poolTarget?: number | null): boolean {
  if (typeof achieved !== "number" || !Number.isFinite(achieved) || achieved <= 0) return false;
  if (typeof poolTarget !== "number" || !Number.isFinite(poolTarget) || poolTarget <= 0) return false;
  return achieved >= poolTarget * LUCKY_THRESHOLD_MULTIPLIER;
}

// Compact difficulty formatter — mirrors SharesPage.formatShareDifficulty exactly.
// Returns "—" (the fleet-standard missing-value affordance) when unavailable,
// matching how other fleet KPI cells (power, avg-temp, slowest) degrade.
function formatFleetDifficulty(value?: number | null): string {
  if (typeof value !== "number" || !Number.isFinite(value) || value <= 0) {
    return "—";
  }
  return value.toLocaleString(undefined, { maximumFractionDigits: 4 });
}

function formatAcceptanceRate(value?: number | null): string {
  if (typeof value !== "number" || !Number.isFinite(value)) return "—";
  return `${(value * 100).toFixed(1)}%`;
}

function poolSnapshotMatches(miner: FleetMiner, snapshot: FleetPoolMinerSnapshot): boolean {
  return snapshot.miner_id === miner.id ||
    snapshot.miner_id === miner.hostname ||
    snapshot.host === miner.ip;
}

function mergePoolStats(miners: FleetMiner[], poolStats: FleetPoolStatsResponse | null): FleetMinerRow[] {
  return miners.map((miner) => {
    const snapshot = poolStats?.stats.miners.find(candidate => poolSnapshotMatches(miner, candidate));
    const submitted = snapshot
      ? snapshot.shares_accepted + snapshot.shares_rejected
      : 0;
    const acceptanceRate = submitted > 0 && snapshot
      ? snapshot.shares_accepted / submitted
      : null;
    return {
      ...miner,
      pool_url: snapshot?.active_pool_url || null,
      pool_worker: null,
      acceptance_rate: acceptanceRate,
    };
  });
}

const STATUS_ORDER: Record<FleetMinerStatus, number> = {
  alive: 0,
  starting: 1,
  dead: 2,
};

// Stale threshold: a miner whose last_seen_ms is older than 5 minutes gets a
// "stale" warning chip. Telemetry-truth contract (Wave 9D9 / 9E): stale ≠ dead.
// A miner can be `alive` per its last snapshot but still have stopped
// reporting. We surface this honestly instead of relabeling status.
const STALE_THRESHOLD_MS = 5 * 60 * 1000;

type StatusFilter = "all" | FleetMinerStatus;

function ipToNumber(ip: string): number {
  return ip
    .split(".")
    .map((part) => Number.parseInt(part, 10))
    .reduce((acc, part) => acc * 256 + (Number.isFinite(part) ? part : 0), 0);
}

function formatFleetHashrate(ghs: number): string {
  if (ghs <= 0) {
    return "0 TH/s";
  }
  if (ghs >= 1_000_000) {
    return `${(ghs / 1_000_000).toFixed(2)} PH/s`;
  }
  if (ghs >= 1_000) {
    return `${(ghs / 1_000).toFixed(2)} TH/s`;
  }
  return `${ghs.toFixed(1)} GH/s`;
}

function formatFleetAverageTemp(totalMiners: number, averageTemp: number): string {
  if (totalMiners <= 0) return "—";
  return `${averageTemp.toFixed(1)} C`;
}

function formatTimestamp(ms: number): string {
  return new Date(ms).toISOString().replace("T", " ").replace(".000Z", "Z");
}

function statusTone(status: FleetMinerStatus): string {
  if (status === "alive") {
    return "success";
  }
  if (status === "starting") {
    return "warning";
  }
  return "danger";
}

function isStale(miner: FleetMiner, nowMs: number): boolean {
  // Don't mark `dead` as stale — `dead` already conveys "we know it's down."
  // Stale applies to miners that still claim alive/starting but haven't
  // refreshed telemetry recently.
  if (miner.status === "dead") return false;
  return nowMs - miner.last_seen_ms > STALE_THRESHOLD_MS;
}

function formatStaleAgo(miner: FleetMiner, nowMs: number): string {
  const ageMs = Math.max(0, nowMs - miner.last_seen_ms);
  const m = Math.floor(ageMs / 60000);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  return `${Math.floor(h / 24)}d ago`;
}

const COLUMNS: FleetColumn[] = [
  {
    key: "hostname",
    label: "Hostname",
    render: (miner) => miner.hostname,
    sortValue: (miner) => miner.hostname.toLowerCase(),
  },
  {
    key: "ip",
    label: "IP",
    render: (miner) => miner.ip,
    sortValue: (miner) => ipToNumber(miner.ip),
  },
  {
    key: "model",
    label: "Model",
    render: (miner) => miner.model,
    sortValue: (miner) => miner.model.toLowerCase(),
  },
  {
    key: "hashrate_ghs",
    label: "Hashrate",
    align: "right",
    render: (miner) => formatFleetHashrate(miner.hashrate_ghs),
    sortValue: (miner) => miner.hashrate_ghs,
  },
  {
    key: "temp_c",
    label: "Temp",
    align: "right",
    render: (miner) => `${miner.temp_c.toFixed(0)} C`,
    sortValue: (miner) => miner.temp_c,
  },
  {
    key: "fan_pwm",
    label: "Fan PWM",
    align: "right",
    render: (miner) => `${miner.fan_pwm}%`,
    sortValue: (miner) => miner.fan_pwm,
  },
  {
    key: "pool_url",
    label: "Pool",
    render: (miner) => miner.pool_url || "—",
    sortValue: (miner) => miner.pool_url ?? "",
  },
  {
    key: "pool_worker",
    label: "Worker",
    sortable: false,
    render: (miner) => miner.pool_worker || "unavailable",
    sortValue: (miner) => miner.pool_worker ?? "",
  },
  {
    key: "acceptance_rate",
    label: "Accept Rate",
    align: "right",
    render: (miner) => formatAcceptanceRate(miner.acceptance_rate),
    sortValue: (miner) => miner.acceptance_rate ?? -1,
  },
  {
    // : pool credit / minimum-work difficulty. Absent until PR-048
    // backend leg ships the field; renders "—" honestly when undefined/null.
    key: "pool_target_difficulty",
    label: "Pool Target",
    align: "right",
    render: (miner) => formatFleetDifficulty(miner.pool_target_difficulty),
    sortValue: (miner) => miner.pool_target_difficulty ?? -1,
  },
  {
    // : locally proven achieved difficulty. Absent until PR-048 backend
    // leg ships the field; renders "—" honestly when undefined/null.
    // The lucky pill fires when achieved ≥ 4× pool target (load-bearing threshold).
    key: "achieved_difficulty",
    label: "Achieved",
    align: "right",
    render: (miner) => formatFleetDifficulty(miner.achieved_difficulty),
    sortValue: (miner) => miner.achieved_difficulty ?? -1,
  },
  {
    key: "status",
    label: "Status",
    render: (miner) => miner.status,
    sortValue: (miner) => STATUS_ORDER[miner.status],
  },
  {
    key: "last_seen_ms",
    label: "Last Seen",
    render: (miner) => formatTimestamp(miner.last_seen_ms),
    sortValue: (miner) => miner.last_seen_ms,
  },
];

export function FleetView() {
  const systemInfo = useMinerStore(s => s.systemInfo);
  const [fleet, setFleet] = useState<FleetResponse | null>(null);
  const [poolStats, setPoolStats] = useState<FleetPoolStatsResponse | null>(null);
  const [identifyState, setIdentifyState] = useState<IdentifyState>({});
  const [error, setError] = useState<string | null>(null);
  const [sort, setSort] = useState<{ key: SortKey; direction: SortDirection }>({
    key: "hostname",
    direction: "asc",
  });
  const [filterText, setFilterText] = useState("");
  const [statusFilter, setStatusFilter] = useState<StatusFilter>("all");

  // Tick once a minute so the "stale Xm ago" chip stays honest even if the
  // fleet snapshot itself hasn't changed.
  const [nowMs, setNowMs] = useState(() => Date.now());
  useEffect(() => {
    const id = window.setInterval(() => setNowMs(Date.now()), 60_000);
    return () => window.clearInterval(id);
  }, []);

  useEffect(() => {
    let cancelled = false;

    listFleetMiners()
      .then((response) => {
        if (!cancelled) {
          setFleet(response);
          setError(null);
        }
      })
      .catch((err) => {
        if (!cancelled) {
          setError(
            err instanceof Error ? err.message : "Fleet inventory unavailable",
          );
        }
      });

    getFleetPoolStats().then((response) => {
      if (!cancelled) {
        setPoolStats(response);
      }
    }).catch(() => {
      if (!cancelled) {
        setPoolStats(null);
      }
    });

    return () => {
      cancelled = true;
    };
  }, []);

  const fleetRows = useMemo(
    () => mergePoolStats(fleet?.miners ?? [], poolStats),
    [fleet, poolStats],
  );

  const filteredMiners = useMemo(() => {
    const miners = fleetRows;
    const q = filterText.trim().toLowerCase();
    return miners.filter((m) => {
      if (statusFilter !== "all" && m.status !== statusFilter) return false;
      if (!q) return true;
      return (
        m.hostname.toLowerCase().includes(q) ||
        m.ip.toLowerCase().includes(q) ||
        m.model.toLowerCase().includes(q)
      );
    });
  }, [fleetRows, filterText, statusFilter]);

  const sortedMiners = useMemo(() => {
    const miners = [...filteredMiners];
    const column =
      COLUMNS.find((candidate) => candidate.key === sort.key) ?? COLUMNS[0];
    const direction = sort.direction === "asc" ? 1 : -1;

    miners.sort((a, b) => {
      const left = column.sortValue(a);
      const right = column.sortValue(b);

      if (typeof left === "number" && typeof right === "number") {
        return (left - right) * direction;
      }

      return (
        String(left).localeCompare(String(right), undefined, {
          numeric: true,
        }) * direction
      );
    });

    return miners;
  }, [filteredMiners, sort]);

  const fleetWindow = useWindowedList<HTMLDivElement>({
    count: sortedMiners.length,
    itemHeight: 49,
    overscan: 10,
    disabled: sortedMiners.length <= 80,
  });
  const visibleMiners = sortedMiners.slice(fleetWindow.start, fleetWindow.end);

  const staleCount = useMemo(() => {
    const miners = fleet?.miners ?? [];
    return miners.filter((m) => isStale(m, nowMs)).length;
  }, [fleet, nowMs]);

  const totals = useMemo(() => {
    const miners = fleet?.miners ?? [];
    const totalHashrate = miners.reduce(
      (sum, miner) => sum + miner.hashrate_ghs,
      0,
    );
    const averageTemp =
      miners.length > 0
        ? miners.reduce((sum, miner) => sum + miner.temp_c, 0) / miners.length
        : 0;

    return {
      total: miners.length,
      alive: miners.filter((miner) => miner.status === "alive").length,
      starting: miners.filter((miner) => miner.status === "starting").length,
      dead: miners.filter((miner) => miner.status === "dead").length,
      totalHashrate,
      averageTemp,
    };
  }, [fleet]);

  const selectSort = (key: SortKey) => {
    setSort((current) => ({
      key,
      direction:
        current.key === key && current.direction === "asc" ? "desc" : "asc",
    }));
  };

  const isLocalMiner = (miner: FleetMinerRow): boolean => {
    const pageHost = typeof window !== "undefined" ? window.location.hostname : "";
    const localHostname = systemInfo?.hostname ?? "";
    return miner.ip === pageHost ||
      miner.hostname === localHostname ||
      miner.id === localHostname;
  };

  const handleIdentify = async (miner: FleetMinerRow) => {
    const local = isLocalMiner(miner);
    setIdentifyState(prev => ({
      ...prev,
      [miner.id]: { status: "pending", message: "Sending identify command..." },
    }));
    try {
      const response = local
        ? await identifyLocalMiner()
        : await identifyRemoteMiner(miner.ip);
      setIdentifyState(prev => ({
        ...prev,
        [miner.id]: {
          status: "sent",
          message: response.message || "Identify command accepted by this unit.",
        },
      }));
    } catch {
      setIdentifyState(prev => ({
        ...prev,
        [miner.id]: local
          ? { status: "failed", message: "Identify command failed on this unit." }
          : {
              status: "fallback",
              message: "Open unit dashboard to identify",
              href: `http://${miner.ip}/`,
            },
      }));
    }
  };

  const isFleetUnavailable = fleet?.source === "api_unavailable";
  const isDemoFleet = fleet?.demo || fleet?.source === "demo";
  const sourceTone = isFleetUnavailable ? "warning" : "info";
  const sourceTitle =
    isFleetUnavailable
      ? "Fleet API unavailable"
      : isDemoFleet
        ? "Demo fleet data"
        : null;
  const sourceMessage =
    fleet?.message ??
    (isFleetUnavailable
      ? "No local fleet rows were returned; demo miners are not shown as live telemetry."
      : isDemoFleet
        ? "This is a static demo fixture, not live miner telemetry."
        : null);

  if (error) {
    return (
      <div className="page-content" data-testid="fleet-view">
        <div className="state-panel warning">
          <div className="state-panel-row">
            <div className="state-panel-main">
              <span className="state-panel-badge">!</span>
              <div className="state-panel-copy">
                <div className="state-panel-title">
                  Fleet inventory unavailable
                </div>
                <div className="state-panel-message">{error}</div>
              </div>
            </div>
          </div>
        </div>
      </div>
    );
  }

  // Before /api/fleet/miners returns anything, render the canonical page
  // skeleton instead of an empty hero + zero KPIs.
  if (fleet === null) {
    return <PageSkeleton data-testid="page-skeleton-fleet" />;
  }

  const minersList = fleetRows;
  const slowestMiner = minersList.length > 0
    ? minersList
        .filter(m => m.status === 'alive')
        .reduce<FleetMinerRow | null>(
          (acc, m) => (acc === null || m.hashrate_ghs < acc.hashrate_ghs ? m : acc),
          null,
        )
    : null;
  return (
    <div className="page-content fleet-view" data-testid="fleet-view">
      <div className="page-hero-strip">
        <div className="page-hero-summary">
          <div className="page-hero-eyebrow">FLEET</div>
          <div className="page-hero-title">Cross-Miner Health</div>
          <div className="page-hero-stat">{totals.alive}/{totals.total} online</div>
          <div className="page-hero-substat">
            {fleet?.source_label
              ? `Source: ${fleet.source_label}`
              : 'Awaiting /api/fleet/miners snapshot.'}
          </div>
        </div>
        <div className="hero-kpi-strip">
          <div className="hero-kpi">
            <div className="kpi-label">Total Hashrate</div>
            <div className="kpi-value">
              <span className="kpi-num-anim">{formatFleetHashrate(totals.totalHashrate)}</span>
            </div>
          </div>
          {/* STD-B-03: the "Total Power" KPI summed a `wall_watts` field that
              is not part of the FleetMiner contract (api/fleet.ts), so it was a
              permanent dead "—". Removed until the field is a real data source. */}
          <div className="hero-kpi">
            <div className="kpi-label">Avg Temp</div>
            <div className="kpi-value">
              <span className="kpi-num-anim">
                {formatFleetAverageTemp(totals.total, totals.averageTemp)}
              </span>
            </div>
          </div>
          <div className="hero-kpi">
            <div className="kpi-label">Slowest Miner</div>
            <div className="kpi-value">
              <span className="kpi-num-anim" style={{ fontSize: '0.95rem' }}>
                {slowestMiner ? slowestMiner.hostname : '—'}
              </span>
            </div>
            {slowestMiner && (
              <div className="kpi-sub">{formatFleetHashrate(slowestMiner.hashrate_ghs)}</div>
            )}
          </div>
        </div>
      </div>

      <section className="section">
      <div className="state-panel info" data-testid="fleet-lan-copy">
        <div className="state-panel-row">
          <div className="state-panel-main">
            <span className="state-panel-badge">i</span>
            <div className="state-panel-copy">
              <div className="state-panel-title">Local network snapshot</div>
              <div className="state-panel-message">
                Local network snapshot — discovered on your LAN + manually added IPs. DCENT_OS has no cloud.
              </div>
            </div>
          </div>
        </div>
      </div>

      {sourceTitle && sourceMessage && (
        <div className={`state-panel ${sourceTone}`} data-testid="fleet-source-notice">
          <div className="state-panel-row">
            <div className="state-panel-main">
              <span className="state-panel-badge">!</span>
              <div className="state-panel-copy">
                <div className="state-panel-title">{sourceTitle}</div>
                <div className="state-panel-message">{sourceMessage}</div>
              </div>
            </div>
          </div>
        </div>
      )}

      <div className="fleet-summary-grid" data-testid="fleet-summary">
        <div className="fleet-summary-card">
          <div className="fleet-summary-label">Miners</div>
          <div className="fleet-summary-value">{totals.total}</div>
          <div className="fleet-summary-note">
            {totals.alive} alive / {totals.starting} starting / {totals.dead}{" "}
            dead
          </div>
        </div>
        <div className="fleet-summary-card">
          <div className="fleet-summary-label">Fleet Hashrate</div>
          <div className="fleet-summary-value">
            {formatFleetHashrate(totals.totalHashrate)}
          </div>
          <div className="fleet-summary-note">
            {fleet?.source_label ?? "local miner snapshot"}
          </div>
        </div>
        <div className="fleet-summary-card" data-testid="fleet-pool-stats-summary">
          <div className="fleet-summary-label">Pool Accept Rate</div>
          <div className="fleet-summary-value">
            {formatAcceptanceRate(poolStats?.stats.acceptance_rate)}
          </div>
          <div className="fleet-summary-note">
            {poolStats?.source ? `/api/fleet/pool-stats - ${poolStats.source}` : "pool stats unavailable"}
          </div>
        </div>
        <div className="fleet-summary-card">
          <div className="fleet-summary-label">Average Temp</div>
          <div className="fleet-summary-value">
            {formatFleetAverageTemp(totals.total, totals.averageTemp)}
          </div>
          <div className="fleet-summary-note">
            generated{" "}
            {fleet ? formatTimestamp(fleet.generated_at_ms) : "pending"}
          </div>
        </div>
      </div>

      <div className="section">
        <div className="section-title section-title-inline">
          <span>Fleet Inventory</span>
          <span className="fleet-source-pill">
            /api/fleet/miners - {fleet?.source_label ?? "pending"}
          </span>
        </div>

        {minersList.length > 0 && (
          <div
            className="fleet-controls"
            data-testid="fleet-controls"
            style={{
              display: "flex",
              flexWrap: "wrap",
              alignItems: "center",
              gap: 10,
              marginBottom: 14,
            }}
          >
            <input
              type="search"
              className="ds-input"
              placeholder="Filter by hostname, IP, or model"
              value={filterText}
              onChange={(e) => setFilterText(e.target.value)}
              aria-label="Filter fleet"
              data-testid="fleet-filter-input"
              style={{ flex: "1 1 220px", minWidth: 0, maxWidth: 340 }}
            />
            <div
              role="group"
              aria-label="Filter by status"
              style={{ display: "flex", gap: 6, flexWrap: "wrap" }}
            >
              {(
                [
                  { key: "all", label: "All", tone: "info" as const, count: totals.total },
                  { key: "alive", label: "Online", tone: "success" as const, count: totals.alive },
                  { key: "starting", label: "Connecting", tone: "warning" as const, count: totals.starting },
                  { key: "dead", label: "Offline", tone: "danger" as const, count: totals.dead },
                ] as const
              ).map((opt) => {
                const active = statusFilter === opt.key;
                return (
                  <button
                    key={opt.key}
                    type="button"
                    className={`ds-chip ds-${opt.tone}`}
                    aria-pressed={active}
                    onClick={() =>
                      setStatusFilter(opt.key as StatusFilter)
                    }
                    data-testid={`fleet-filter-${opt.key}`}
                    style={{
                      cursor: "pointer",
                      opacity: active ? 1 : 0.62,
                      borderWidth: active ? 2 : 1,
                      padding: active ? "3px 10px" : "4px 11px",
                    }}
                  >
                    {opt.label}
                    <span style={{ marginLeft: 6, opacity: 0.85 }}>
                      {opt.count}
                    </span>
                  </button>
                );
              })}
            </div>
            {staleCount > 0 && (
              <Tooltip content="One or more miners haven't refreshed telemetry recently. The row shows the last known snapshot — it is real past data, not a live reading.">
                <span
                  className="ds-chip ds-warning"
                  aria-label={`${staleCount} miner${staleCount === 1 ? "" : "s"} with stale telemetry`}
                  data-testid="fleet-stale-summary"
                >
                  <span className="ds-dot" aria-hidden="true" />
                  {staleCount} stale
                </span>
              </Tooltip>
            )}
          </div>
        )}

        <div
          ref={fleetWindow.containerRef}
          onScroll={fleetWindow.onScroll}
          className="fleet-table-wrap"
        >
          <table className="fleet-table" aria-label="Fleet miners">
            <thead>
              <tr>
                {COLUMNS.map((column) => {
                  const active = sort.key === column.key;
                  const directionLabel =
                    active && sort.direction === "asc"
                      ? "ascending"
                      : "descending";

                  return (
                    <th
                      key={column.key}
                      scope="col"
                      aria-sort={column.sortable === false ? undefined : active ? directionLabel : "none"}
                      className={
                        column.align === "right" ? "align-right" : undefined
                      }
                    >
                      {column.sortable === false ? (
                        <span>{column.label}</span>
                      ) : (
                        <button
                          type="button"
                          className="fleet-sort-button"
                          data-testid={`fleet-sort-${column.key}`}
                          aria-label={`Sort fleet by ${column.label}`}
                          onClick={() => selectSort(column.key)}
                        >
                          <span>{column.label}</span>
                          <span
                            className={`fleet-sort-indicator ${active ? "active" : ""}`}
                          >
                            {active
                              ? sort.direction === "asc"
                                ? "up"
                                : "down"
                              : "sort"}
                          </span>
                        </button>
                      )}
                    </th>
                  );
                })}
                <th scope="col">Identify</th>
              </tr>
            </thead>
            <tbody>
              {sortedMiners.length === 0 ? (
                <tr data-testid="fleet-empty-row">
                  <td colSpan={COLUMNS.length + 1}>
                    {minersList.length === 0 ? (
                      <EmptyState
                        illustration={<NoFleetIllustration />}
                        title="No miners discovered yet"
                        hint={
                          isFleetUnavailable
                            ? "No fleet rows are available from the local API. Add a miner manually or check that your fleet endpoint is reachable."
                            : "Add a miner manually or run network discovery."
                        }
                        data-testid="fleet-empty-state"
                      />
                    ) : (
                      <EmptyState
                        title="No miners match your filter"
                        hint={
                          statusFilter !== "all" || filterText
                            ? `${minersList.length} miner${minersList.length === 1 ? "" : "s"} hidden by current filter.`
                            : "Try clearing the filter."
                        }
                        action={{
                          label: "Clear filter",
                          onClick: () => {
                            setFilterText("");
                            setStatusFilter("all");
                          },
                        }}
                        data-testid="fleet-empty-filtered"
                      />
                    )}
                  </td>
                </tr>
              ) : (
                <>
                {fleetWindow.padTop > 0 && (
                  <tr aria-hidden="true">
                    <td colSpan={COLUMNS.length + 1} style={{ height: fleetWindow.padTop, padding: 0, borderBottom: 0 }} />
                  </tr>
                )}
                {visibleMiners.map((miner) => {
                  const stale = isStale(miner, nowMs);
                  return (
                    <tr
                      key={miner.id}
                      data-testid={`fleet-row-${miner.id}`}
                      className={stale ? "fleet-row-stale" : undefined}
                    >
                      {COLUMNS.map((column) => (
                        <td
                          key={column.key}
                          data-label={column.label}
                          className={
                            column.align === "right" ? "align-right" : undefined
                          }
                        >
                          {column.key === "status" ? (
                            // Keep the legacy .status-chip text so Cypress'
                            // text-content assertions (alive / starting /
                            // dead) keep passing. The .ds-chip below adds
                            // the design-system pulsing-dot affordance for
                            // alive miners.
                            <span style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
                              <span
                                className={`status-chip ${statusTone(miner.status)}`}
                              >
                                {column.render(miner)}
                              </span>
                              {stale && (
                                <Tooltip content={`Telemetry last refreshed ${formatStaleAgo(miner, nowMs)}. Status shown is the last snapshot.`}>
                                  <span
                                    className="ds-chip ds-warning"
                                    aria-label={`Stale telemetry — last seen ${formatStaleAgo(miner, nowMs)}. Status is the last known snapshot.`}
                                    data-testid={`fleet-row-stale-${miner.id}`}
                                    style={{ fontSize: "0.6rem", padding: "2px 7px" }}
                                  >
                                    <span className="ds-dot" aria-hidden="true" />
                                    stale {formatStaleAgo(miner, nowMs)}
                                  </span>
                                </Tooltip>
                              )}
                            </span>
                          ) : column.key === "achieved_difficulty" ? (
                            // : render achieved difficulty + lucky pill
                            // when achieved ≥ 4× pool target (LUCKY_THRESHOLD_MULTIPLIER).
                            // The lucky pill is purely additive — it never appears when the
                            // field is absent, and it never overrides the "—" affordance.
                            <span style={{ display: "inline-flex", alignItems: "center", gap: 6, justifyContent: "flex-end" }}>
                              <span className="shares-td-mono">
                                {column.render(miner)}
                              </span>
                              {isLuckyShare(miner.achieved_difficulty, miner.pool_target_difficulty) && (
                                <Tooltip content={`Achieved ${formatFleetDifficulty(miner.achieved_difficulty)} ≥ ${LUCKY_THRESHOLD_MULTIPLIER}× pool target ${formatFleetDifficulty(miner.pool_target_difficulty)}`}>
                                  <span
                                    className="shares-lucky-pill"
                                    aria-label={`Lucky share: achieved difficulty ${formatFleetDifficulty(miner.achieved_difficulty)} is at least ${LUCKY_THRESHOLD_MULTIPLIER}× the pool target of ${formatFleetDifficulty(miner.pool_target_difficulty)}`}
                                    data-testid={`fleet-row-lucky-${miner.id}`}
                                  >
                                    Lucky
                                  </span>
                                </Tooltip>
                              )}
                            </span>
                          ) : (
                            column.render(miner)
                          )}
                        </td>
                      ))}
                      <td data-label="Identify">
                        {(() => {
                          const state = identifyState[miner.id] ?? { status: "idle" as const, message: "" };
                          if (state.status === "fallback" && state.href) {
                            return (
                              <a
                                href={state.href}
                                target="_blank"
                                rel="noreferrer"
                                data-testid={`fleet-identify-fallback-${miner.id}`}
                              >
                                {state.message}
                              </a>
                            );
                          }
                          return (
                            <div style={{ display: "grid", gap: 4, justifyItems: "start" }}>
                              <button
                                type="button"
                                className="btn btn-secondary"
                                data-testid={`fleet-identify-${miner.id}`}
                                disabled={state.status === "pending"}
                                onClick={() => void handleIdentify(miner)}
                                style={{ padding: "4px 10px", fontSize: "0.72rem" }}
                              >
                                {state.status === "pending" ? "Identifying..." : "Identify"}
                              </button>
                              {state.message && (
                                <span
                                  className={`small-tag ${state.status === "sent" ? "good" : state.status === "failed" ? "warn" : ""}`}
                                  data-testid={`fleet-identify-status-${miner.id}`}
                                >
                                  {state.message}
                                </span>
                              )}
                            </div>
                          );
                        })()}
                      </td>
                    </tr>
                  );
                })}
                {fleetWindow.padBottom > 0 && (
                  <tr aria-hidden="true">
                    <td colSpan={COLUMNS.length + 1} style={{ height: fleetWindow.padBottom, padding: 0, borderBottom: 0 }} />
                  </tr>
                )}
                </>
              )}
            </tbody>
          </table>
        </div>
      </div>
      </section>
    </div>
  );
}
