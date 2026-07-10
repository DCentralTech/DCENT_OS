/// <reference types="cypress" />

function seedBackupSettings(win: Window) {
  win.localStorage.clear();
  win.localStorage.setItem('dcentos-settings', JSON.stringify({
    setupComplete: true,
    mode: 'standard',
    minerName: 'Firmware Miner',
  }));
  win.localStorage.setItem('dcentos-current-page', 'settings/backup');
  win.localStorage.setItem('dcentos-nav-standard', 'settings/backup');
}

function stubBackupAndUpgradeStatus() {
  cy.intercept('GET', '/api/config/backup/manifest', {
    statusCode: 200,
    body: {
      status: 'ok',
      read_only: true,
      content_collected: false,
      restore_supported: false,
      daemon_config_export_supported: true,
      dashboard_preferences_export_supported: true,
      sources: [],
      redaction_policy: {
        content_included: false,
        secret_key_patterns: ['password', 'token'],
        notes: [],
      },
      limitations: [],
    },
  }).as('backupManifest');
  cy.intercept('GET', '/api/system/upgrade/status', {
    statusCode: 200,
    body: {
      status: 'ok',
      read_only: true,
      state: 'idle',
      stage_root: '/tmp/dcentos-upgrade',
      stage_root_present: true,
      staged_package_count: 0,
      staged_packages: [],
      upgrade_stage: null,
      bootcount: null,
      bootlimit: null,
      boot_slot: 'A',
      limitations: [],
    },
  }).as('upgradeStatus');
}

describe('Firmware update flow vocabulary', () => {
  it('validates and schedules a package without claiming it has flashed or booted', () => {
    stubBackupAndUpgradeStatus();
    cy.intercept('POST', '/api/system/upgrade', {
      statusCode: 200,
      body: {
        status: 'scheduled',
        message: 'Signature verified; target preflight passed; flash scheduled for the inactive slot.',
        staged_path: '/tmp/dcentos-upgrade/dcentos-public-beta.tar',
        filename: 'dcentos-public-beta.tar',
      },
    }).as('uploadFirmware');

    cy.visit('/#/settings/backup', { onBeforeLoad: seedBackupSettings });
    cy.wait('@backupManifest');
    cy.wait('@upgradeStatus');

    cy.contains('.section-title', 'Firmware Update').scrollIntoView().should('be.visible');
    cy.contains('staged').should('be.visible');
    cy.contains('Boot observed').should('be.visible');
    cy.contains('committed rollback state is not claimed', { matchCase: false }).should('be.visible');

    cy.get('input[type="file"][accept=".tar"]').selectFile({
      contents: Cypress.Buffer.from('fake signed sysupgrade package'),
      fileName: 'dcentos-public-beta.tar',
      mimeType: 'application/x-tar',
    }, { force: true });
    cy.contains('button', 'Validate + Schedule').click();
    cy.contains('.ab-confirm-body', 'schedule inactive-slot flashing').should('be.visible');
    cy.contains('.ab-confirm-actions button', 'Confirm').click();

    cy.wait('@uploadFirmware');
    cy.contains('.cp-alert', 'flash scheduled for the inactive slot').should('be.visible');
    cy.contains('.cp-alert', /flashed|booted|complete/i).should('not.exist');
  });
});
