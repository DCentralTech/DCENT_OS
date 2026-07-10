import React from 'react';
import type { GlossaryKey } from '../../utils/glossary';
import { StatusPill, type StatusPillState } from './StatusPill';
import { InfoDot } from './Tooltip';

export interface PageHeaderStatus {
  state: StatusPillState;
  label?: string;
  pulse?: boolean;
}

export interface PageHeaderAction {
  label: string;
  onClick: () => void;
  tone?: 'primary' | 'secondary' | 'ghost' | 'danger';
}

interface PageHeaderProps {
  title: string;
  description: string;
  status?: PageHeaderStatus;
  primaryAction?: PageHeaderAction;
  infoKey?: GlossaryKey;
}

export function PageHeader({
  title,
  description,
  status,
  primaryAction,
  infoKey,
}: PageHeaderProps) {
  return (
    <header className="standard-page-header">
      <div className="standard-page-header-main">
        <div className="standard-page-header-title-row">
          <h1 className="page-title">{title}</h1>
          {infoKey && <InfoDot term={infoKey} size={15} placement="bottom" />}
        </div>
        <p className="page-desc">{description}</p>
      </div>
      {(status || primaryAction) && (
        <div className="standard-page-header-rail">
          {status && (
            <StatusPill status={status.state} label={status.label} pulse={status.pulse} />
          )}
          {primaryAction && (
            <button
              type="button"
              className={`ds-btn ${primaryAction.tone ?? 'secondary'} sm`}
              onClick={primaryAction.onClick}
            >
              {primaryAction.label}
            </button>
          )}
        </div>
      )}
    </header>
  );
}
