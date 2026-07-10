import React from 'react';
import { usePatchBay } from '../../hooks/usePatchBay';

export function PatchBayPanel() {
  const { definitions, rules, updateRule, triggerTest } = usePatchBay();

  const armedCount = Object.values(rules).filter(r => r?.enabled).length;

  return (
    <div className="hacker-inspector">
      <header className="hacker-inspector-header">
        <div className="hacker-inspector-title-group">
          <div className="hacker-inspector-eyebrow">// patch bay</div>
          <h2 className="hacker-inspector-title">Event Effect Router</h2>
        </div>
        <div className="hacker-inspector-actions">
          <span className="hacker-inspector-status">{armedCount}/{definitions.length} ARMED</span>
        </div>
      </header>

      <div className="hacker-inspector-toolbar">
        <span className="adv-hint" style={{ fontSize: '0.74rem' }}>
          Route runtime events into browser-side effects. Good for building your own warning language without touching the mining path.
        </span>
      </div>

      <div className="hacker-inspector-body">
        <div className="pbp-list">
          {definitions.map(definition => {
            const rule = rules[definition.id];
            return (
              <div key={definition.id} className="glass-card pbp-lane">
                <div className="pbp-lane-head">
                  <div>
                    <div className="pbp-lane-name">{definition.label}</div>
                    <div className="pbp-lane-desc">{definition.description}</div>
                  </div>
                  <div className="pbp-lane-actions">
                    <label className="control-option pbp-opt">
                      <input
                        type="checkbox"
                        checked={rule.enabled}
                        onChange={event => updateRule(definition.id, { ...rule, enabled: event.target.checked })}
                      />
                      Armed
                    </label>
                    <button className="btn btn-secondary" onClick={() => triggerTest(definition.id)}>
                      Test
                    </button>
                  </div>
                </div>

                <div className="pbp-effects">
                  {(['toast', 'marker', 'pulse', 'beep', 'freeze'] as const).map(effect => (
                    <label key={effect} className="control-option pbp-opt">
                      <input
                        type="checkbox"
                        checked={rule.effects[effect]}
                        onChange={event => updateRule(definition.id, {
                          ...rule,
                          effects: {
                            ...rule.effects,
                            [effect]: event.target.checked,
                          },
                        })}
                      />
                      {effect}
                    </label>
                  ))}
                </div>
              </div>
            );
          })}
        </div>
      </div>

      <footer className="hacker-inspector-footer">
        <div className="hacker-inspector-stats">
          <span>{definitions.length} event lanes</span>
          <span>{armedCount} armed</span>
        </div>
      </footer>
    </div>
  );
}
