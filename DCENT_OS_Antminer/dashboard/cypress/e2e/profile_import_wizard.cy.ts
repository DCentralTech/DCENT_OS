// Cypress e2e — Profile-import wizard happy path.
//
// TODO(wave-9): wire Cypress runner. The dashboard package.json
// currently does not declare cypress; the test file is shipped in
// place so the runner can pick it up once the test harness is
// configured.
//
// Manual run (after wave-9 wiring):
//   cd DCENT_OS_Antminer/dashboard
//   npm install --save-dev cypress
//   npx cypress run --spec cypress/e2e/profile_import_wizard.cy.ts
//
// Pre-conditions:
//   - dcentrald running with profiles registry mounted (W8-D backend)
//   - Profile fixture available: BHB42601 17 preset rows
//
// Wire shape contract (mirrors W8-D.md):
//   GET    /api/profiles/silicon                  -> [{id, miner_model, hashboard, chip, source_class, preset_count}, ...]
//   POST   /api/profiles/silicon/import           -> {id, path, loaded}
//   POST   /api/profiles/silicon/import-json      -> {id, path, loaded}
//   PUT    /api/profiles/silicon/active           -> {status, model, hashboard, profile_id, note}

/// <reference types="cypress" />

describe('Profile-import wizard — 5-step happy path', () => {
  beforeEach(() => {
    // Stub the empty-list state so the page renders deterministically.
    cy.intercept('GET', '/api/profiles/silicon', {
      statusCode: 200,
      body: [],
    }).as('listProfiles');

    // Fake-import echoes back a deterministic id.
    cy.intercept('POST', '/api/profiles/silicon/import-json', {
      statusCode: 201,
      body: {
        id: 'antminer_s9__BHB42601__bm1387__operator_confirmed',
        path: '/etc/dcentrald/profiles.d/operator/imported.json',
        loaded: 1,
      },
    }).as('importJson');

    // Stub the post-import list refresh.
    cy.intercept('GET', '/api/profiles/silicon/*', (req) => {
      req.reply({
        statusCode: 200,
        body: {
          schema_version: 1,
          miner_model: 'antminer_s9',
          hashboard: 'BHB42601',
          chip: 'bm1387',
          source_class: 'operator_confirmed',
          presets: Array.from({ length: 17 }, (_, i) => ({
            step: i,
            freq_mhz: 400 + i * 10,
            voltage_v: 8.7 + i * 0.02,
          })),
        },
      });
    });

    cy.visit('/#/profiles/import');
  });

  it('drops a profile JSON, walks the 5 steps, and lands 17 BHB42601 rows', () => {
    // Build a fake profile JSON in-browser
    const fakeBundle = {
      schema_version: 1,
      miner_model: 'antminer_s9',
      hashboard: 'BHB42601',
      chip: 'bm1387',
      source_class: 'vendor_extracted',
      presets: Array.from({ length: 17 }, (_, i) => ({
        step: i,
        freq_mhz: 400 + i * 10,
        voltage_v: 8.7 + i * 0.02,
      })),
      metadata: {
        secure_boot_set_seen: false,
        hashcore_root_hash_seen: false,
      },
    };

    // Step 1 — drop the JSON file
    cy.get('input[type="file"]').selectFile({
      contents: Cypress.Buffer.from(JSON.stringify(fakeBundle)),
      fileName: 'profile.json',
      mimeType: 'application/json',
    }, { force: true });

    // Step 2 — DetectionResults shows the parsed bundle
    cy.contains('Hashboard').should('exist');
    cy.get('input[value="BHB42601"]').should('exist');
    cy.contains('17').should('exist'); // 17 preset rows

    cy.contains('button', 'Next').click();

    // Step 3 — Diff. With no active profile, all 17 are "added".
    cy.contains('+ 17 added').should('exist');
    cy.contains('button', 'Next').click();

    // Step 4 — SourceClassSelect. Operator downgrades to operator_confirmed.
    cy.contains('Operator confirmed').click();
    cy.contains('button', 'Next').click();

    // Step 5 — Apply. Click "Apply import".
    cy.contains('button', 'Apply import').click();

    cy.wait('@importJson').its('request.body').should((body) => {
      expect(body).to.have.property('bundle');
      expect(body.bundle.miner_model).to.eq('antminer_s9');
      expect(body.bundle.hashboard).to.eq('BHB42601');
      expect(body.bundle.chip).to.eq('bm1387');
      expect(body.bundle.source_class).to.eq('operator_confirmed');
      expect(body.bundle.presets).to.have.length(17);
    });

    cy.contains('Bundle written and registry reloaded.').should('exist');
    cy.contains('Profile id: antminer_s9__BHB42601__bm1387__operator_confirmed').should('exist');
    cy.contains('loaded 1').should('exist');
  });

  it('surfaces a 400 error when the registry rejects (e.g., SECURE_BOOT_SET tainted)', () => {
    cy.intercept('POST', '/api/profiles/silicon/import-json', {
      statusCode: 400,
      body: { error: 'SECURE_BOOT_SET present in metadata; bundle refused' },
    }).as('reject');

    const tainted = {
      schema_version: 1,
      miner_model: 'antminer_s21',
      hashboard: 'AML-S11board',
      chip: 'bm1368',
      source_class: 'vendor_extracted',
      presets: [{ step: 0, freq_mhz: 600, voltage_v: 1.5 }],
      metadata: { secure_boot_set_seen: true },
    };

    cy.get('input[type="file"]').selectFile({
      contents: Cypress.Buffer.from(JSON.stringify(tainted)),
      fileName: 'tainted.json',
      mimeType: 'application/json',
    }, { force: true });

    // Walk to apply
    cy.contains('button', 'Next').click(); // detection -> diff
    cy.contains('button', 'Next').click(); // diff -> source
    cy.contains('Operator confirmed').click();
    cy.contains('button', 'Next').click(); // source -> apply
    cy.contains('button', 'Apply import').click();

    cy.wait('@reject');
    cy.contains('SECURE_BOOT_SET').should('exist');
    cy.contains('Import refused').should('exist');
  });
});
