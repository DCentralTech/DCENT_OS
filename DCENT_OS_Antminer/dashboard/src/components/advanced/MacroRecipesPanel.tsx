import React, { useMemo, useState } from 'react';
import { ActionButton } from '../common/ActionButton';
import { CliHint } from './CliHint';
import { echoCli } from '../../hooks/useCliEcho';
import { useFlightRecorder } from '../../hooks/useFlightRecorder';
import { useActiveHardware } from '../../hooks/useActiveHardware';
import { api } from '../../api/client';

type RecipeId =
  | 'evidence-pack'
  | 'diagnostic-sweep'
  | 'maintenance-snapshot'
  | 'share-flow-snapshot'
  | 'locate-and-mark'
  | 'safe-restart';

interface RecipeState {
  status: 'idle' | 'running' | 'success' | 'error';
  message: string;
  lastRunAt: number | null;
}

interface MacroRecipe {
  id: RecipeId;
  title: string;
  description: string;
  confirm: string;
  run: () => Promise<string>;
}

const INITIAL_RECIPE_STATE: RecipeState = {
  status: 'idle',
  message: '',
  lastRunAt: null,
};

function downloadJson(filename: string, payload: unknown) {
  const json = JSON.stringify(payload, null, 2);
  const blob = new Blob([json], { type: 'application/json' });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement('a');
  anchor.href = url;
  anchor.download = filename;
  document.body.appendChild(anchor);
  anchor.click();
  document.body.removeChild(anchor);
  URL.revokeObjectURL(url);
}

function formatRunTime(timestamp: number | null) {
  if (!timestamp) {
    return 'Never';
  }

  return new Date(timestamp).toLocaleTimeString('en-US', {
    hour12: false,
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  });
}

export function MacroRecipesPanel() {
  const { activeChain } = useActiveHardware();
  const { freeze, resume, exportBundle, addMarker, recordAction } = useFlightRecorder();
  const [chain, setChain] = useState<number | undefined>(undefined);
  const [hashreportMinutes, setHashreportMinutes] = useState(5);
  const [recipeStates, setRecipeStates] = useState<Record<RecipeId, RecipeState>>({
    'evidence-pack': { ...INITIAL_RECIPE_STATE },
    'diagnostic-sweep': { ...INITIAL_RECIPE_STATE },
    'maintenance-snapshot': { ...INITIAL_RECIPE_STATE },
    'share-flow-snapshot': { ...INITIAL_RECIPE_STATE },
    'locate-and-mark': { ...INITIAL_RECIPE_STATE },
    'safe-restart': { ...INITIAL_RECIPE_STATE },
  });

  const updateRecipe = (id: RecipeId, next: Partial<RecipeState>) => {
    setRecipeStates(prev => ({
      ...prev,
      [id]: {
        ...prev[id],
        ...next,
      },
    }));
  };

  const runRecipe = async (id: RecipeId, label: string, fn: () => Promise<string>) => {
    const startedAt = Date.now();
    updateRecipe(id, { status: 'running', message: 'Running...', lastRunAt: startedAt });
    echoCli(`recipe run ${id}${chain == null ? '' : ` --chain ${chain}`}`);
      recordAction('macro_recipe_started', { id, label, chain: chain ?? 'all', hashreportMinutes });
    try {
      const message = await fn();
      updateRecipe(id, { status: 'success', message, lastRunAt: Date.now() });
      recordAction('macro_recipe_completed', { id, label, message });
    } catch (error: unknown) {
      const message = error instanceof Error ? error.message : 'Recipe failed';
      updateRecipe(id, { status: 'error', message, lastRunAt: Date.now() });
      recordAction('macro_recipe_failed', { id, label, message });
    }
  };

  const recipes = useMemo<MacroRecipe[]>(() => ([
    {
      id: 'evidence-pack' as const,
      title: 'Capture Evidence Pack',
      description: 'Drop a marker, freeze the recorder, export the full session JSON, then resume capture.',
      confirm: 'Capture and download a full evidence pack from the current session?',
      run: async () => {
        addMarker('macro: evidence pack');
        freeze();
        exportBundle();
        resume();
        return 'Evidence pack exported and recorder resumed.';
      },
    },
    {
      id: 'diagnostic-sweep' as const,
      title: 'Start Diagnostic Sweep',
      description: 'Kick off hash report, chip health, and board health snapshots for the selected chain scope.',
      confirm: 'Start all three diagnostics for the selected chain scope?',
      run: async () => {
        const request = {
          chain,
          duration_minutes: hashreportMinutes,
        };
        const [hashreport, chiphealth, boardhealth] = await Promise.all([
          api.startHashReport(request),
          api.startChipHealth({ chain }),
          api.startBoardHealth({ chain }),
        ]);
        const summary = {
          hashreport: hashreport.test_id,
          chiphealth: chiphealth.test_id,
          boardhealth: boardhealth.test_id,
        };
        recordAction('macro_diagnostic_sweep_started', summary);
        return `Started diagnostics: ${Object.values(summary).join(', ')}`;
      },
    },
    {
      id: 'maintenance-snapshot' as const,
      title: 'Maintenance Health Snapshot',
      description: 'Run network, PSU, and FPGA troubleshoot endpoints and download the combined JSON snapshot.',
      confirm: 'Run a maintenance snapshot and download the combined troubleshooting results?',
      run: async () => {
        const [network, psu, fpga] = await Promise.all([
          api.troubleshootNetwork(),
          api.troubleshootPsu(),
          api.troubleshootFpga(),
        ]);
        const payload = {
          exportedAt: new Date().toISOString(),
          network,
          psu,
          fpga,
        };
        downloadJson(`dcentos-maintenance-snapshot-${Date.now()}.json`, payload);
        return 'Maintenance snapshot exported.';
      },
    },
    {
      id: 'share-flow-snapshot' as const,
      title: 'Share Flow Snapshot',
      description: 'Capture recent correlated share history from the miner and export it for offline analysis.',
      confirm: 'Export recent share-flow history as JSON?',
      run: async () => {
        const history = await api.getShareHistory();
        const historyEvents = history.events ?? [];
        downloadJson(`dcentos-share-flow-${Date.now()}.json`, {
          exportedAt: new Date().toISOString(),
          events: historyEvents,
        });
        return `Exported ${historyEvents.length} recent share events.`;
      },
    },
    {
      id: 'locate-and-mark' as const,
      title: 'Locate And Mark',
      description: 'Add a journal marker, then trigger the miner locate pattern to physically identify the unit.',
      confirm: 'Trigger the locate pattern and mark the journal?',
      run: async () => {
        addMarker('macro: locate and mark');
        await api.triggerLocate();
        return 'Locate pattern triggered.';
      },
    },
    {
      id: 'safe-restart' as const,
      title: 'Safe Daemon Restart',
      description: 'Journal the action, drop a marker, then request a daemon restart through the normal API.',
      confirm: 'Restart the mining daemon now?',
      run: async () => {
        addMarker('macro: safe daemon restart');
        await api.restart();
        return 'Restart requested. Expect a short telemetry gap while dcentrald restarts.';
      },
    },
  ]), [addMarker, chain, exportBundle, freeze, hashreportMinutes, recordAction, resume]);

  const runningCount = Object.values(recipeStates).filter(s => s?.status === 'running').length;

  return (
    <div className="hacker-inspector">
      <header className="hacker-inspector-header">
        <div className="hacker-inspector-title-group">
          <div className="hacker-inspector-eyebrow">// macro recipes</div>
          <h2 className="hacker-inspector-title">Reusable Safe Sequences</h2>
        </div>
        <div className="hacker-inspector-actions">
          <span className={`hacker-inspector-status ${runningCount > 0 ? 'warning' : ''}`}>
            {runningCount > 0 ? `${runningCount} RUNNING` : 'READY'}
          </span>
        </div>
      </header>

      <div className="hacker-inspector-body">
      <div className="register-inspector ds-card-hover mr-controls-card">
        <div className="advanced-inline-actions mr-controls-row">
          <div>
            <label className="advanced-control-label" htmlFor="macro-recipe-chain">Chain Scope</label>
            <select
              id="macro-recipe-chain"
              value={chain ?? 'all'}
              onChange={event => setChain(event.target.value === 'all' ? undefined : Number(event.target.value))}
            >
              <option value="all">All chains</option>
              <option value={6}>Chain 6</option>
              <option value={7}>Chain 7</option>
              <option value={8}>Chain 8</option>
            </select>
          </div>
          <div>
            <label className="advanced-control-label" htmlFor="macro-recipe-hashreport">Hash Report Duration</label>
            <select
              id="macro-recipe-hashreport"
              value={hashreportMinutes}
              onChange={event => setHashreportMinutes(Number(event.target.value))}
            >
              <option value={1}>1 minute</option>
              <option value={5}>5 minutes</option>
              <option value={10}>10 minutes</option>
            </select>
          </div>
          <div className="mr-context">
            Active chain context: <span className="mr-context-chain">{activeChain}</span>. Recipes with no explicit chain selection run against all chains.
          </div>
        </div>
      </div>

      <div className="mr-list">
        {recipes.map(recipe => {
          const state = recipeStates[recipe.id];
          const tone = state.status === 'error' ? 'var(--red)' : state.status === 'success' ? 'var(--green)' : state.status === 'running' ? 'var(--yellow)' : 'var(--text-dim)';
          return (
            <div key={recipe.id} className="register-inspector ds-card-hover mr-recipe">
              <div className="mr-recipe-head">
                <div>
                  <div className="mr-recipe-title">{recipe.title}</div>
                  <div className="mr-recipe-desc">{recipe.description}</div>
                </div>
                <ActionButton
                  label={state.status === 'running' ? 'Running...' : 'Run Recipe'}
                  onClick={() => runRecipe(recipe.id, recipe.title, recipe.run)}
                  confirm={recipe.confirm}
                  disabled={state.status === 'running'}
                />
              </div>
              <CliHint cmd={`recipe run ${recipe.id}${chain == null ? '' : ` --chain ${chain}`}`} />

              <div className="mr-recipe-foot">
                <span style={{ color: tone }}>
                  {state.status === 'idle' ? 'Ready' : state.message || state.status}
                </span>
                <span className="mr-last-run">Last run: {formatRunTime(state.lastRunAt)}</span>
              </div>
            </div>
          );
        })}
      </div>
      </div>

      <footer className="hacker-inspector-footer">
        <div className="hacker-inspector-stats">
          <span>{recipes.length} recipes</span>
          <span>scope: {chain == null ? 'all chains' : `chain ${chain}`}</span>
          <span>active: chain {activeChain}</span>
        </div>
      </footer>
    </div>
  );
}
