// Step 1 — drag/drop firmware tarball OR JSON profile.
//
// W8-D does NOT ship a server-side preview endpoint; the wizard
// calls `dcent import preview` shape on its own by parsing JSON
// client-side (works for the .json bundle case). Tarball files are
// passed through to /import as multipart and the backend parses +
// returns the staged bundle id; we then GET that id back to render
// detection results (Step 2).

import React, { useCallback, useRef, useState } from 'react';
import { siliconProfilesApi, type SiliconProfileBundle } from '../../api/profiles-silicon';

interface UploadProps {
  onParsed: (bundle: SiliconProfileBundle, sourceFile: File) => void;
  onError: (msg: string) => void;
}

const ACCEPT_HINT = '.json, .tar, .tar.gz, .tgz';

export function Upload({ onParsed, onError }: UploadProps) {
  const [dragOver, setDragOver] = useState(false);
  const [parsing, setParsing] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  const handleFile = useCallback(async (file: File) => {
    setParsing(true);
    try {
      const isJson = /\.json$/i.test(file.name) || file.type === 'application/json';
      if (isJson) {
        const text = await file.text();
        const parsed = JSON.parse(text);
        // Bundles may be wrapped in {bundle: {...}} or top-level.
        const bundle = (parsed && typeof parsed === 'object' && 'bundle' in parsed)
          ? (parsed as { bundle: SiliconProfileBundle }).bundle
          : parsed as SiliconProfileBundle;
        if (!bundle || typeof bundle !== 'object' || !('schema_version' in bundle)) {
          onError('That JSON file does not look like a silicon profile bundle (no schema_version).');
          return;
        }
        onParsed(bundle, file);
      } else {
        // Tarball — server-side parse via /import (multipart). We
        // upload, then GET the resulting bundle by id.
        const result = await siliconProfilesApi.importMultipart(file);
        const bundle = await siliconProfilesApi.get(result.id);
        onParsed(bundle, file);
      }
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Upload or parse failed';
      onError(msg);
    } finally {
      setParsing(false);
    }
  }, [onParsed, onError]);

  const dropZoneId = 'profile-upload-dropzone';
  const inputId = 'profile-upload-input';

  return (
    <div className="section">
      {/* Visually-hidden label for the file input — SR reads this on focus */}
      <label htmlFor={inputId} className="sr-only">
        Upload firmware tarball or profile JSON ({ACCEPT_HINT})
      </label>
      {/* Drop-zone: role="button" + aria-controls ties it to the real input.
          Keyboard-operable (Enter/Space) per WCAG 2.1 SC 4.1.2. */}
      <div
        id={dropZoneId}
        role="button"
        aria-label={parsing ? 'Parsing file, please wait' : `Upload firmware tarball or profile JSON. Accepts ${ACCEPT_HINT}. Press Enter or Space to browse files.`}
        aria-busy={parsing}
        tabIndex={0}
        onClick={() => inputRef.current?.click()}
        onKeyDown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); inputRef.current?.click(); } }}
        onDragOver={(e) => { e.preventDefault(); setDragOver(true); }}
        onDragLeave={() => setDragOver(false)}
        onDrop={(e) => {
          e.preventDefault();
          setDragOver(false);
          const file = e.dataTransfer.files?.[0];
          if (file) void handleFile(file);
        }}
        style={{
          border: `2px dashed ${dragOver ? 'var(--accent, #FAA500)' : 'rgba(255,255,255,0.18)'}`,
          borderRadius: 12,
          padding: '40px 24px',
          textAlign: 'center',
          background: dragOver ? 'rgba(247,147,26,0.06)' : 'rgba(18,18,26,0.6)',
          color: 'var(--text)',
          cursor: 'pointer',
          transition: 'background 0.15s, border-color 0.15s',
        }}
      >
        <div style={{ fontSize: '0.9rem', fontWeight: 700, marginBottom: 8 }}>
          {parsing ? 'Parsing...' : 'Drop firmware tarball or profile JSON here'}
        </div>
        <div style={{ fontSize: '0.75rem', color: 'var(--text-secondary, #8b8b9e)' }}>
          {parsing ? 'Hold tight — extracting bundle on the daemon.' : `Accepts ${ACCEPT_HINT}`}
        </div>
        {/* status region — SR announces parse progress without assertive interruption */}
        <div role="status" aria-live="polite" className="sr-only">
          {parsing ? 'Parsing file, please wait.' : ''}
        </div>
        <input
          id={inputId}
          ref={inputRef}
          type="file"
          accept={ACCEPT_HINT}
          aria-hidden="true"
          tabIndex={-1}
          style={{ display: 'none' }}
          onChange={(e) => {
            const file = e.target.files?.[0];
            if (file) void handleFile(file);
          }}
        />
      </div>
      <div style={{ fontSize: '0.7rem', color: 'var(--text-dim, #6E6E80)', marginTop: 8 }}>
        Tarball uploads pass through <code>POST /api/profiles/silicon/import</code> for server-side parse.
        JSON bundles parse locally so you can edit chip / hashboard / source class before submitting.
      </div>
    </div>
  );
}
