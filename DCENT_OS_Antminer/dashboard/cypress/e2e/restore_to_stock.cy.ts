// Cypress e2e — Restore-to-Stock multi-step modal.
//
// TODO(wave-9): wire Cypress runner. The dashboard package.json
// currently does not declare cypress; the test file is shipped in
// place so the runner can pick it up once the test harness is
// configured.
//
// Manual run (after wave-9 wiring):
//   cd DCENT_OS_Antminer/dashboard
//   npm install --save-dev cypress
//   npx cypress run --spec cypress/e2e/restore_to_stock.cy.ts
//
// Wire shape contract (mirrors W8-F.md):
//   POST /api/system/restore-to-stock/preflight  -> {status, safety_findings, slot_plan, staged_sha256}
//   POST /api/system/restore-to-stock            -> {status, safety_findings, ...}  (default dry-run)
//   GET  /api/system/restore-to-stock/status     -> {last_preflight, last_flash_attempt, scheduled_reboot_at_ms}
//
// Operator flow:
//   1. Open Danger Zone page, click button.
//   2. Type wrong serial -> Next disabled.
//   3. Type correct serial -> Step 2 OK.
//   4. Stage tarball -> preflight Critical -> Step 3 locked.
//   5. Stage clean tarball -> Step 3 OK -> Confirm step.
//   6. Submit dry-run -> assert backend received correct body shape.

/// <reference types="cypress" />

const FAKE_SERIAL = 'S9-LAB-0001';

function visitDangerZone() {
  cy.intercept('GET', '/api/system/info', {
    statusCode: 200,
    body: {
      firmware: 'dcentos',
      version: '0.5.0',
      model: 'Antminer S9',
      hostname: FAKE_SERIAL,
      mac: '00:11:22:33:44:55',
      uptime_s: 3600,
      chip_type: 'bm1387',
      chip_count: 189,
      chain_count: 3,
      mode: 'standard',
      hashrate_ghs: 12000,
      api_version: '2.0',
      board: 'BHB42601',
      soc: 'zynq',
      hardware: { miner_serial: FAKE_SERIAL, control_board: 's9', hb_type: 'BHB42601', chip_type: 'bm1387', psu_model: 'APW3' },
    },
  }).as('systemInfo');

  cy.visit('/#/system');
}

function openRestoreModal() {
  cy.contains('button', /(?:flash to stock firmware|restore to stock)/i).click();
}

function acknowledgeBreakerRisk() {
  cy.get('[data-testid="restore-breaker-ack"]')
    .check({ force: true })
    .should('be.checked');
}

function clickNext() {
  cy.contains('button', 'Next')
    .scrollIntoView()
    .should('not.be.disabled')
    .click({ force: true });
}

function setControlledInput(selector: string, value: string) {
  cy.get(selector).then(($input) => {
    const input = $input[0] as HTMLInputElement;
    const valueSetter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set;
    valueSetter?.call(input, value);
    input.dispatchEvent(new Event('input', { bubbles: true }));
    input.dispatchEvent(new Event('change', { bubbles: true }));
  });
  cy.get(selector).should('have.value', value);
}

function enterSerial(serial: string) {
  setControlledInput('#restore-serial-input', serial);
}

function armFlashSlider() {
  setControlledInput('#restore-slider-input', '100');
  cy.contains(/Slider armed/i).should('exist');
}

function clickFlashNow() {
  cy.contains('button', /flash now/i)
    .scrollIntoView()
    .should('not.be.disabled')
    .click({ force: true });
}

function clickBackdropOutsideDialog() {
  cy.get('[data-testid="overlay-backdrop"]').trigger('click', { force: true });
}

describe('Restore-to-Stock — multi-step confirm', () => {
  it('blocks Next when serial is wrong, advances on match', () => {
    visitDangerZone();
    openRestoreModal();

    // Step 1 — toggle ack + leave default 1 board
    acknowledgeBreakerRisk();
    clickNext();

    // Step 2 — type wrong serial
    enterSerial('WRONG-SERIAL');
    cy.contains('button', 'Next').should('be.disabled');

    // Type correct serial
    enterSerial(FAKE_SERIAL);
    cy.contains('button', 'Next').should('not.be.disabled');
  });

  it('locks at preflight step when CRITICAL findings are returned', () => {
    visitDangerZone();
    cy.intercept('POST', '/api/system/upgrade', {
      statusCode: 200,
      body: { staged_path: '/tmp/dcentos-upgrade/abc/stock.tar.gz', size: 16 * 1024 * 1024 },
    }).as('stage');
    cy.intercept('POST', '/api/system/restore-to-stock/preflight', {
      statusCode: 200,
      body: {
        status: 'rejected_critical_safety_finding',
        staged_sha256: 'c3b77476bfc640ed' + 'a'.repeat(48),
        safety_findings: [
          {
            id: 'DCENT-2026-010',
            severity: 'critical',
            detector: 'SECURE_BOOT_SET',
            description: 'A113D eFuse trigger blob detected',
            no_override: true,
          },
        ],
      },
    }).as('preflight');

    openRestoreModal();
    acknowledgeBreakerRisk();
    clickNext();
    enterSerial(FAKE_SERIAL);
    clickNext();

    // Step 3 — stage
    cy.get('input[type="file"]').selectFile({
      contents: Cypress.Buffer.from('fake stock tarball'),
      fileName: 'stock.tar.gz',
      mimeType: 'application/gzip',
    }, { force: true });
    cy.contains('button', /stage firmware/i).click();
    cy.wait('@stage');
    cy.wait('@preflight');

    clickNext();

    // Step 4 — preflight is critical, "Next" must be disabled.
    cy.contains('Critical').should('exist');
    cy.contains('SECURE_BOOT_SET').should('exist');
    cy.contains('button', 'Next').should('be.disabled');
  });

  it('requires HIGH ack checkbox before advancing past preflight', () => {
    visitDangerZone();
    cy.intercept('POST', '/api/system/upgrade', {
      statusCode: 200,
      body: { staged_path: '/tmp/dcentos-upgrade/xyz/stock.tar.gz', size: 1024 },
    }).as('stage');
    cy.intercept('POST', '/api/system/restore-to-stock/preflight', {
      statusCode: 200,
      body: {
        status: 'preflight_ok',
        staged_sha256: 'a'.repeat(64),
        safety_findings: [
          {
            id: 'DCENT-2026-009',
            severity: 'high',
            detector: 'atlas@anthill.farm',
            description: 'VNish vendor SSH key',
            no_override: false,
          },
        ],
      },
    }).as('preflight');

    openRestoreModal();
    acknowledgeBreakerRisk();
    clickNext();
    enterSerial(FAKE_SERIAL);
    clickNext();
    cy.get('input[type="file"]').selectFile({
      contents: Cypress.Buffer.from('fake'),
      fileName: 'stock.tar.gz',
      mimeType: 'application/gzip',
    }, { force: true });
    cy.contains('button', /stage firmware/i).click();
    cy.wait('@preflight');
    clickNext();

    // High finding is shown, but "Next" still disabled until ack.
    cy.contains('button', 'Next').should('be.disabled');
    cy.contains('label', /reviewed the high-severity findings/i).click();
    cy.contains('button', 'Next').should('not.be.disabled');
  });

  it('walks the full clean path and submits the correct body shape', () => {
    visitDangerZone();
    cy.intercept('POST', '/api/system/upgrade', {
      statusCode: 200,
      body: { staged_path: '/tmp/dcentos-upgrade/clean/stock.tar.gz', size: 1024 },
    });
    cy.intercept('POST', '/api/system/restore-to-stock/preflight', {
      statusCode: 200,
      body: {
        status: 'preflight_ok',
        staged_sha256: 'b'.repeat(64),
        safety_findings: [],
      },
    }).as('preflight');
    cy.intercept('POST', '/api/system/restore-to-stock', {
      statusCode: 200,
      body: {
        status: 'scheduled',
        staged_sha256: 'b'.repeat(64),
        safety_findings: [],
        backup_path: '/data/restore-backup-1717182000/',
        reboot_at_ms: Date.now() + 30000,
      },
    }).as('submit');

    openRestoreModal();
    acknowledgeBreakerRisk();
    clickNext();
    enterSerial(FAKE_SERIAL);
    clickNext();
    cy.get('input[type="file"]').selectFile({
      contents: Cypress.Buffer.from('fake'),
      fileName: 'stock.tar.gz',
      mimeType: 'application/gzip',
    }, { force: true });
    cy.contains('button', /stage firmware/i).click();
    cy.wait('@preflight');
    clickNext();
    cy.contains('Preflight clean').should('exist');
    clickNext();

    // Step 5 — phrase + slider
    setControlledInput('#restore-phrase-input', 'RESTORE TO STOCK');
    armFlashSlider();

    clickFlashNow();

    cy.wait('@submit').its('request.body').should((body) => {
      expect(body.confirm).to.eq(true);
      expect(body.confirm_string_typed).to.eq('RESTORE TO STOCK');
      expect(body.operator_serial_typed).to.eq(FAKE_SERIAL);
      expect(body.acknowledge_breaker_warning).to.eq(true);
      expect(body.hashboard_count_to_use).to.be.a('number');
      expect(body.stock_firmware_staged_path).to.contain('/tmp/dcentos-upgrade/');
      //  W9-G (R5-MEDIUM): the dashboard MUST round
      // `acknowledge_high_findings` to the wire so the backend can
      // refuse `confirm:true` against unacknowledged HIGH findings.
      // Clean-path stub returns zero findings, so the bool defaults to
      // its modal-state value (false). The presence of the key matters
      // more than the value here.
      expect(body).to.have.property('acknowledge_high_findings');
    });

    cy.contains('Reboot scheduled').should('exist');
  });

  it('asserts manual recovery copy replaces the auto_recovery promise (R1-C4 / R5-H1)', () => {
    visitDangerZone();
    openRestoreModal();

    // Step 1 must spell out the manual fw_setenv recovery — the old
    // "U-Boot auto_recovery returns DCENT_OS on the next power cycle"
    // promise is operator-misleading.
    cy.contains(/manual recovery required/i).should('exist');
    cy.contains(/fw_setenv bootslot/i).should('exist');
    cy.contains(/auto.?recovery returns dcent_os on the next power cycle/i).should('not.exist');
  });

  it('renders the breaker banner on every multi-step screen (R5-H4)', () => {
    visitDangerZone();
    cy.intercept('POST', '/api/system/upgrade', {
      statusCode: 200,
      body: { staged_path: '/tmp/dcentos-upgrade/sticky/stock.tar.gz', size: 1024 },
    });
    cy.intercept('POST', '/api/system/restore-to-stock/preflight', {
      statusCode: 200,
      body: { status: 'preflight_ok', staged_sha256: 'a'.repeat(64), safety_findings: [] },
    }).as('preflight');

    openRestoreModal();

    // Step 1 already shows the warning prominently; the banner kicks in
    // from step 2 onward (TypeSerial/NandBackup/SafetyPreflight/Confirm).
    acknowledgeBreakerRisk();
    clickNext();

    // Step 2 — banner present.
    cy.contains(/breaker-stressing and noisy/i).should('exist');
    enterSerial(FAKE_SERIAL);
    clickNext();

    // Step 3 — banner present.
    cy.contains(/breaker-stressing and noisy/i).should('exist');
    cy.get('input[type="file"]').selectFile({
      contents: Cypress.Buffer.from('fake'),
      fileName: 'stock.tar.gz',
      mimeType: 'application/gzip',
    }, { force: true });
    cy.contains('button', /stage firmware/i).click();
    cy.wait('@preflight');

    clickNext();
    // Step 4 — banner present.
    cy.contains(/breaker-stressing and noisy/i).should('exist');
  });

  it('R5\'-M3 (W10-C): backdrop + ESC dismiss only on steps 0-1, locked on 2+', () => {
    visitDangerZone();
    cy.intercept('POST', '/api/system/upgrade', {
      statusCode: 200,
      body: { staged_path: '/tmp/dcentos-upgrade/dismiss/stock.tar.gz', size: 1024 },
    });
    cy.intercept('POST', '/api/system/restore-to-stock/preflight', {
      statusCode: 200,
      body: { status: 'preflight_ok', staged_sha256: 'a'.repeat(64), safety_findings: [] },
    }).as('preflight');

    // Step 0 (Status) — ESC closes the modal.
    openRestoreModal();
    cy.contains('Restore to stock').should('be.visible');
    cy.get('body').type('{esc}');
    cy.get('[role="dialog"]').should('not.exist');
    cy.visit('/#/system');

    // Step 0 (Status) — backdrop click also closes.
    openRestoreModal();
    cy.contains('Restore to stock').should('be.visible');
    // The fixed-position backdrop is the outermost portaled div.
    // Click outside the dialog content to trigger backdrop dismissal.
    // Click ~20px outside the top-left corner of the dialog.
    clickBackdropOutsideDialog();
    cy.get('[role="dialog"]').should('not.exist');
    cy.visit('/#/system');

    // Re-open and walk to step 2 (Stage).
    openRestoreModal();
    acknowledgeBreakerRisk();
    clickNext(); // -> step 1 (Serial)

    // Step 1 (Serial) — still dismissible because step < 2.
    cy.get('body').type('{esc}');
    cy.get('[role="dialog"]').should('not.exist');
    cy.visit('/#/system');

    // Re-open and advance to step 2 (Stage).
    openRestoreModal();
    acknowledgeBreakerRisk();
    clickNext(); // -> step 1
    enterSerial(FAKE_SERIAL);
    clickNext(); // -> step 2 (Stage)

    // Step 2 — ESC must NOT close the modal.
    cy.contains(/stage firmware \+ safety preflight/i).should('be.visible');
    cy.get('body').type('{esc}');
    cy.contains(/stage firmware \+ safety preflight/i).should('be.visible');

    // Step 2 — backdrop click must NOT close the modal.
    clickBackdropOutsideDialog();
    cy.contains(/stage firmware \+ safety preflight/i).should('be.visible');

    // Step 2 — explicit Cancel button still works.
    cy.contains('button', 'Cancel').click();
    cy.get('[role="dialog"]').should('not.exist');
  });

  it('R5\'-#24 (W10-C): renders flash_failed.reason in red callout on RebootScheduled', () => {
    visitDangerZone();
    cy.intercept('POST', '/api/system/upgrade', {
      statusCode: 200,
      body: { staged_path: '/tmp/dcentos-upgrade/ff/stock.tar.gz', size: 1024 },
    });
    cy.intercept('POST', '/api/system/restore-to-stock/preflight', {
      statusCode: 200,
      body: { status: 'preflight_ok', staged_sha256: 'a'.repeat(64), safety_findings: [] },
    }).as('preflight');
    cy.intercept('POST', '/api/system/restore-to-stock', {
      statusCode: 200,
      body: {
        status: 'rejected_flash_failed',
        staged_sha256: 'a'.repeat(64),
        safety_findings: [],
        backup_path: '/data/restore-backup-1717182000/',
        state_detail: {
          phase: 'flash_failed',
          reason: 'flash_erase: device or resource busy',
          backup_path: '/data/restore-backup-1717182000/',
        },
      },
    }).as('submit');

    openRestoreModal();
    acknowledgeBreakerRisk();
    clickNext();
    enterSerial(FAKE_SERIAL);
    clickNext();
    cy.get('input[type="file"]').selectFile({
      contents: Cypress.Buffer.from('fake'),
      fileName: 'stock.tar.gz',
      mimeType: 'application/gzip',
    }, { force: true });
    cy.contains('button', /stage firmware/i).click();
    cy.wait('@preflight');
    clickNext();
    clickNext();

    // Step 4 — phrase + slider, then submit.
    setControlledInput('#restore-phrase-input', 'RESTORE TO STOCK');
    armFlashSlider();
    clickFlashNow();
    cy.wait('@submit');

    // Step 5 — flash_failed callout visible with reason + recovery cite.
    cy.contains(/flash failed:/i).should('be.visible');
    cy.contains('flash_erase: device or resource busy').should('be.visible');
    cy.contains(/STOCK_BOOT_HARVEST_PROCEDURE\.md/).should('be.visible');
  });

  it('NAND backup step copy says dd runs server-side at confirm (R5-H3)', () => {
    visitDangerZone();
    cy.intercept('POST', '/api/system/upgrade', {
      statusCode: 200,
      body: { staged_path: '/tmp/dcentos-upgrade/nand/stock.tar.gz', size: 1024 },
    });
    cy.intercept('POST', '/api/system/restore-to-stock/preflight', {
      statusCode: 200,
      body: { status: 'preflight_ok', staged_sha256: 'a'.repeat(64), safety_findings: [] },
    }).as('preflight');

    openRestoreModal();
    acknowledgeBreakerRisk();
    clickNext();
    enterSerial(FAKE_SERIAL);
    clickNext();

    cy.get('input[type="file"]').selectFile({
      contents: Cypress.Buffer.from('fake'),
      fileName: 'stock.tar.gz',
      mimeType: 'application/gzip',
    }, { force: true });
    cy.contains('button', /stage firmware/i).click();
    cy.wait('@preflight');

    // Heading now says "Stage firmware + safety preflight" (NAND dd
    // claim removed; the actual partition dump is documented as
    // running at the confirm step).
    cy.contains(/stage firmware \+ safety preflight/i).should('exist');
    cy.contains(/runs server-side at the confirm step/i).should('exist');
    cy.contains(/staged \+ preflight complete/i).should('not.exist');
  });

  // -------------------------------------------------------------------------
  //  W11-C
  // -------------------------------------------------------------------------

  it('W11-C C1 (A5\'\'-OPS-MED-3): mid-flash phase rendering follows /status', () => {
    // Mock /status with a mutable phase that the test will rotate
    // through nand_backup_running -> staging -> flash_running ->
    // flash_succeeded. The dashboard polls every 1s and renders the
    // current phase + spinner; on the terminal phase polling stops.
    type StatusPhase =
      | 'nand_backup_running'
      | 'staging'
      | 'flash_running'
      | 'flash_succeeded';
    const phaseScript: StatusPhase[] = [
      'nand_backup_running',
      'staging',
      'flash_running',
      'flash_succeeded',
    ];
    let callIdx = 0;
    let submitted = false;
    cy.intercept('GET', '/api/system/restore-to-stock/status', (req) => {
      if (!submitted) {
        req.reply({
          statusCode: 200,
          body: {
            state: 'idle',
            last_safety_findings: [],
            transitions: 0,
            last_backup_fw_setenv_present: true,
          },
        });
        return;
      }
      const phase = phaseScript[Math.min(callIdx, phaseScript.length - 1)];
      callIdx += 1;
      const body: Record<string, unknown> = {
        state: phase,
        last_safety_findings: [],
        transitions: callIdx,
        last_backup_fw_setenv_present: true,
        state_detail:
          phase === 'flash_succeeded'
            ? { phase, completed_at_ms: Date.now(), backup_path: '/data/restore-backup-1717182000/' }
            : phase === 'flash_running' || phase === 'staging'
              ? { phase, backup_path: '/data/restore-backup-1717182000/' }
              : { phase },
      };
      req.reply({ statusCode: 200, body });
    }).as('status');

    cy.intercept('POST', '/api/system/upgrade', {
      statusCode: 200,
      body: { staged_path: '/tmp/dcentos-upgrade/phase/stock.tar.gz', size: 1024 },
    });
    cy.intercept('POST', '/api/system/restore-to-stock/preflight', {
      statusCode: 200,
      body: { status: 'preflight_ok', staged_sha256: 'a'.repeat(64), safety_findings: [] },
    }).as('preflight');
    cy.intercept('POST', '/api/system/restore-to-stock', (req) => {
      submitted = true;
      req.reply({
        statusCode: 200,
        body: {
          status: 'scheduled',
          staged_sha256: 'a'.repeat(64),
          safety_findings: [],
          backup_path: '/data/restore-backup-1717182000/',
          reboot_at_ms: Date.now() + 30000,
        },
      });
    }).as('submit');

    visitDangerZone();

    // Walk through to the RebootScheduled step.
    openRestoreModal();
    acknowledgeBreakerRisk();
    clickNext();
    enterSerial(FAKE_SERIAL);
    clickNext();
    cy.get('input[type="file"]').selectFile({
      contents: Cypress.Buffer.from('fake'),
      fileName: 'stock.tar.gz',
      mimeType: 'application/gzip',
    }, { force: true });
    cy.contains('button', /stage firmware/i).click();
    cy.wait('@preflight');
    clickNext();
    clickNext();
    setControlledInput('#restore-phrase-input', 'RESTORE TO STOCK');
    armFlashSlider();
    clickFlashNow();
    cy.wait('@submit');

    // Phase 1: nand_backup_running rendered.
    cy.get('[data-testid="restore-phase-row"]', { timeout: 5000 }).should('be.visible');
    cy.contains(/Backing up NAND/i).should('exist');

    // Wait for staging label.
    cy.contains(/Staging stock firmware/i, { timeout: 5000 }).should('exist');

    // Wait for flash_running label.
    cy.contains(/Flashing inactive slot/i, { timeout: 5000 }).should('exist');

    // Terminal phase — flash_succeeded label visible, spinner gone.
    cy.contains(/Flash succeeded/i, { timeout: 5000 }).should('exist');
  });

  it('W11-C C2 (A5\'\'-OPS-MED-2): per-reason recovery decision tree', () => {
    visitDangerZone();
    cy.intercept('POST', '/api/system/upgrade', {
      statusCode: 200,
      body: { staged_path: '/tmp/dcentos-upgrade/rec/stock.tar.gz', size: 1024 },
    });
    cy.intercept('POST', '/api/system/restore-to-stock/preflight', {
      statusCode: 200,
      body: { status: 'preflight_ok', staged_sha256: 'a'.repeat(64), safety_findings: [] },
    }).as('preflight');
    cy.intercept('POST', '/api/system/restore-to-stock', {
      statusCode: 200,
      body: {
        status: 'rejected_flash_failed',
        staged_sha256: 'a'.repeat(64),
        safety_findings: [],
        backup_path: '/data/restore-backup-1717182000/',
        state_detail: {
          phase: 'flash_failed',
          reason: 'fw_setenv: command not found',
          backup_path: '/data/restore-backup-1717182000/',
        },
      },
    }).as('submit');
    // Mid-flight /status returns same flash_failed (terminal — polling stops).
    cy.intercept('GET', '/api/system/restore-to-stock/status', {
      statusCode: 200,
      body: {
        state: 'flash_failed',
        last_safety_findings: [],
        transitions: 1,
        last_backup_fw_setenv_present: false,
        state_detail: {
          phase: 'flash_failed',
          reason: 'fw_setenv: command not found',
          backup_path: '/data/restore-backup-1717182000/',
        },
      },
    });

    openRestoreModal();
    acknowledgeBreakerRisk();
    clickNext();
    enterSerial(FAKE_SERIAL);
    clickNext();
    cy.get('input[type="file"]').selectFile({
      contents: Cypress.Buffer.from('fake'),
      fileName: 'stock.tar.gz',
      mimeType: 'application/gzip',
    }, { force: true });
    cy.contains('button', /stage firmware/i).click();
    cy.wait('@preflight');
    clickNext();
    clickNext();
    setControlledInput('#restore-phrase-input', 'RESTORE TO STOCK');
    armFlashSlider();
    clickFlashNow();
    cy.wait('@submit');

    // fw_setenv reason -> serial-console (Option B) recovery copy.
    cy.get('[data-testid="restore-recovery-guidance"]')
      .should('have.attr', 'data-severity', 'serial')
      .and('contain.text', 'Option B');
  });

  it('W11-C C3 (A5\'\'-OPS-MED-1): pre-confirm fw_setenv warning at Step 0 + Confirm', () => {
    // Mock /status saying a prior backup did NOT include fw_setenv.
    cy.intercept('GET', '/api/system/restore-to-stock/status', {
      statusCode: 200,
      body: {
        state: 'idle',
        last_safety_findings: [],
        transitions: 0,
        last_backup_fw_setenv_present: false,
      },
    }).as('status');

    visitDangerZone();
    cy.intercept('POST', '/api/system/upgrade', {
      statusCode: 200,
      body: { staged_path: '/tmp/dcentos-upgrade/c3/stock.tar.gz', size: 1024 },
    });
    cy.intercept('POST', '/api/system/restore-to-stock/preflight', {
      statusCode: 200,
      body: { status: 'preflight_ok', staged_sha256: 'a'.repeat(64), safety_findings: [] },
    }).as('preflight');

    openRestoreModal();

    // Step 0 — prior-backup warning visible.
    cy.get('[data-testid="status-prior-fwsetenv-warning"]').should('be.visible');
    cy.contains(/Prior backup on this daemon lacked fw_setenv/i).should('exist');

    // Walk to Confirm step (4).
    acknowledgeBreakerRisk();
    clickNext();
    enterSerial(FAKE_SERIAL);
    clickNext();
    cy.get('input[type="file"]').selectFile({
      contents: Cypress.Buffer.from('fake'),
      fileName: 'stock.tar.gz',
      mimeType: 'application/gzip',
    }, { force: true });
    cy.contains('button', /stage firmware/i).click();
    cy.wait('@preflight');
    clickNext();
    clickNext();

    // Confirm step — pre-confirm warning visible BEFORE the slider.
    cy.get('[data-testid="confirm-fwsetenv-warning"]').should('be.visible');
    cy.contains(/Wire the USB-TTL serial cable/i).should('exist');
  });

  it('W11-C C4: pre-flight checklist renders 8 static items at Step 0', () => {
    visitDangerZone();
    openRestoreModal();

    // Static fallback (endpoint deferred to wave-12) — 8 checklist
    // items present.
    cy.get('[data-testid="restore-preflight-checklist"]').should('exist');
    cy.contains(/Pre-flight checklist/i).should('exist');
    cy.get('[data-testid="restore-preflight-item"]').should('have.length', 8);
    // Sanity: a couple of marquee labels are present.
    cy.contains(/setsid available on PATH/i).should('exist');
    cy.contains(/At least 250 MiB free/i).should('exist');
  });

  // -------------------------------------------------------------------------
  //  W12-C — dynamic pre-flight endpoint
  // -------------------------------------------------------------------------

  it('W12-C: preflight-checks endpoint — all present renders 9 green rows', () => {
    cy.intercept('GET', '/api/system/restore-to-stock/preflight-checks', {
      statusCode: 200,
      body: {
        setsid_path: '/usr/bin/setsid',
        revert_script_path: '/usr/sbin/revert_to_stock_s9.sh',
        fw_setenv_path: '/usr/sbin/fw_setenv',
        tar_path: '/bin/tar',
        nandwrite_path: '/usr/sbin/nandwrite',
        flash_erase_path: '/usr/sbin/flash_erase',
        data_free_mib: 412,
        platform_signature: 'zynq-am1-bm1387',
        platform_supported: true,
        platform_verified_revertable: true,
        all_present: true,
      },
    }).as('preflightChecks');
    visitDangerZone();
    openRestoreModal();
    cy.wait('@preflightChecks');

    // Dynamic checklist visible; static fallback NOT visible.
    cy.get('[data-testid="restore-preflight-checklist-dynamic"]').should('exist');
    cy.get('[data-testid="restore-preflight-checklist"]').should('not.exist');

    // All 9 rows in OK state.
    [
      'setsid',
      'revert_script',
      'fw_setenv',
      'tar',
      'nandwrite',
      'flash_erase',
      'data_free',
      'platform_supported',
      'platform_verified',
    ].forEach((key) => {
      cy.get(`[data-testid="restore-preflight-row-${key}"]`)
        .should('have.attr', 'data-state', 'ok');
    });

    // Path strings rendered for the path probes.
    cy.contains('/usr/bin/setsid').should('exist');
    cy.contains('/usr/sbin/revert_to_stock_s9.sh').should('exist');
    cy.contains('/usr/sbin/fw_setenv').should('exist');
    cy.contains('/bin/tar').should('exist');
    cy.contains('/usr/sbin/nandwrite').should('exist');
    cy.contains('/usr/sbin/flash_erase').should('exist');
    cy.contains('412 MiB free / 250 MiB required').should('exist');
    cy.contains('zynq-am1-bm1387').should('exist');

    // Overall ready badge.
    cy.get('[data-testid="restore-preflight-all-present"]')
      .should('have.attr', 'data-state', 'ready')
      .and('contain.text', 'READY');
  });

  it('W12-C: preflight-checks endpoint — setsid missing flags red row', () => {
    cy.intercept('GET', '/api/system/restore-to-stock/preflight-checks', {
      statusCode: 200,
      body: {
        setsid_path: null,
        revert_script_path: '/usr/sbin/revert_to_stock_s9.sh',
        fw_setenv_path: '/usr/sbin/fw_setenv',
        tar_path: '/bin/tar',
        nandwrite_path: '/usr/sbin/nandwrite',
        flash_erase_path: '/usr/sbin/flash_erase',
        data_free_mib: 412,
        platform_signature: 'zynq-am1-bm1387',
        platform_supported: true,
        platform_verified_revertable: true,
        all_present: false,
      },
    }).as('preflightChecks');
    visitDangerZone();
    openRestoreModal();
    cy.wait('@preflightChecks');

    // setsid row red, others green.
    cy.get('[data-testid="restore-preflight-row-setsid"]')
      .should('have.attr', 'data-state', 'fail')
      .and('contain.text', 'missing');
    [
      'revert_script',
      'fw_setenv',
      'tar',
      'nandwrite',
      'flash_erase',
      'data_free',
      'platform_supported',
      'platform_verified',
    ].forEach((key) => {
      cy.get(`[data-testid="restore-preflight-row-${key}"]`)
        .should('have.attr', 'data-state', 'ok');
    });

    // Overall MISSING PIECES badge.
    cy.get('[data-testid="restore-preflight-all-present"]')
      .should('have.attr', 'data-state', 'missing')
      .and('contain.text', 'MISSING PIECES');
  });

  it('W13-D (A2\'-#1): progress streaming during flash_running renders writer log lines', () => {
    visitDangerZone();

    // Mock /status with flash_running phase + recent_log_lines that
    // mimic typical revert_to_stock_s9.sh output. The dashboard polls
    // every 1s; we return the same body each time so the live pane
    // renders the streamed lines for the operator. Last line is
    // load-bearing — the cypress assertion below targets it.
    cy.intercept('GET', '/api/system/restore-to-stock/status', {
      statusCode: 200,
      body: {
        state: 'flash_running',
        last_safety_findings: [],
        transitions: 5,
        last_backup_fw_setenv_present: true,
        state_detail: {
          phase: 'flash_running',
          backup_path: '/data/restore-backup-1',
        },
        recent_log_lines: [
          'Erasing /dev/mtd7 0x00000000 0x05c00000',
          'Erase Total 1 Units',
          'Performing Flash Erase of length 131072 at offset 0x5be0000',
          'Flash erase done',
          'Writing /dev/mtd7 from stock UBI image',
          '[err] nandwrite: argument 0 has 0 bytes',
          'Writing data to block 1 at offset 0x20000',
          'Writing data to block 2 at offset 0x40000',
          'Writing data to block 3 at offset 0x60000',
          'Wrote 12345/12345 OK',
        ],
      },
    }).as('status');
    cy.intercept('POST', '/api/system/upgrade', {
      statusCode: 200,
      body: { staged_path: '/tmp/dcentos-upgrade/w13d/stock.tar.gz', size: 1024 },
    });
    cy.intercept('POST', '/api/system/restore-to-stock/preflight', {
      statusCode: 200,
      body: { status: 'preflight_ok', staged_sha256: 'a'.repeat(64), safety_findings: [] },
    }).as('preflight');
    cy.intercept('POST', '/api/system/restore-to-stock', {
      statusCode: 200,
      body: {
        status: 'scheduled',
        staged_sha256: 'a'.repeat(64),
        safety_findings: [],
        backup_path: '/data/restore-backup-1',
        reboot_at_ms: Date.now() + 30000,
      },
    }).as('submit');

    // Walk the wizard to the RebootScheduled step.
    openRestoreModal();
    acknowledgeBreakerRisk();
    clickNext();
    enterSerial(FAKE_SERIAL);
    clickNext();
    cy.get('input[type="file"]').selectFile(
      {
        contents: Cypress.Buffer.from('fake'),
        fileName: 'stock.tar.gz',
        mimeType: 'application/gzip',
      },
      { force: true },
    );
    cy.contains('button', /stage firmware/i).click();
    cy.wait('@preflight');
    clickNext();
    clickNext();
    setControlledInput('#restore-phrase-input', 'RESTORE TO STOCK');
    armFlashSlider();
    clickFlashNow();
    cy.wait('@submit');

    // Phase row visible (flash_running).
    cy.get('[data-testid="restore-phase-row"]', { timeout: 5000 })
      .should('be.visible');

    // Live progress pane renders.
    cy.get('[data-testid="restore-progress-stream"]', { timeout: 5000 })
      .should('be.visible')
      .and('contain.text', 'Writer output (live)')
      // Last-10 slice means the last line MUST be visible.
      .and('contain.text', 'Wrote 12345/12345 OK');
  });
});
