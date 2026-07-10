// @vitest-environment jsdom

import { describe, expect, it } from 'vitest';

import * as common from './index';

describe('common shared component surface', () => {
  it('exports the promoted cross-family primitives from one stable module', () => {
    const expected = [
      'ActionButton',
      'AlertBanner',
      'CommandPalette',
      'DcentOsLogo',
      'EmptyState',
      'FindMyMiner',
      'HardwareDetectionState',
      'InfoBanner',
      'LiveAsicVisual',
      'ModeSwitch',
      'OverlayDialog',
      'PageHeader',
      'Skeleton',
      'Sparkline',
      'StatePanel',
      'StatusPill',
      'SupportTierBadge',
      'SvgChart',
      'ToastContainer',
      'Tooltip',
    ];

    for (const name of expected) {
      expect(common, `${name} should be exported from components/common`).toHaveProperty(name);
    }
  });
});

