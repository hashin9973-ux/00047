export function createNativeModsPage({ invoke, esc, toast, modal, htmlModal, getView }) {
  const $ = (selector) => document.querySelector(selector);
  let lifecycleRefreshTimer = null;
  let refreshInFlight = false;

  function bindActions(root) {
    root.querySelectorAll('[data-native-action]').forEach((button) => {
      if (button.dataset.nativeBound === 'true') return;
      button.dataset.nativeBound = 'true';
      button.addEventListener('click', async () => handleAction(button));
    });
  }

  async function renderNativeMods() {
    const host = $('#view-native-mods');
    host.innerHTML = '<div class="plugins-head"><div><h2>Native Windows Mods</h2><p class="mu">Manage native x64 DLL modules for supported Windows shell processes.</p></div><div class="plugins-actions"><button class="btn" data-native-action="folder">Open native mods folder</button><button class="btn" data-native-action="refresh">Refresh targets</button><button class="btn" data-native-action="build">Build from source...</button><button class="btn primary" data-native-action="install">+ Install native mod ZIP...</button></div></div>' +
      '<div class="native-warning card glass"><b>High-impact native code</b><p class="mu">A native mod runs inside Explorer or another listed shell process with that process permissions. A faulty or hostile DLL can hang or crash it. Only enable source you trust. The loader accepts x64 DLLs and a fixed allow-list of Windows shell processes, not arbitrary targets.</p></div>' +
      '<div id="native-signature-policy" class="card glass native-signature-policy"><div><b>Trusted Authenticode signatures</b><p class="mu">Loading signature policy...</p></div></div>' +
      '<div id="native-targets"><p class="mu">Discovering supported shell processes...</p></div>' +
      '<div id="native-mod-list"><p class="mu">Loading native mods...</p></div><div id="native-mod-errors"></div>';
    bindActions(host);
    await refreshData();
  }

  async function refreshData() {
    if (refreshInFlight) return;
    refreshInFlight = true;
    if (lifecycleRefreshTimer !== null) {
      clearTimeout(lifecycleRefreshTimer);
      lifecycleRefreshTimer = null;
    }
    const host = $('#view-native-mods');
    try {
      const info = await invoke('native_mod_refresh');
      if (getView() !== 'native-mods') return;
      const signatureRequired = info.requireTrustedSignature === true;
      $('#native-signature-policy').innerHTML =
        '<div class="native-signature-policy-copy"><div><b>Trusted Authenticode signatures</b><span class="native-signature-status ' +
        (signatureRequired ? 'is-required' : 'is-optional') + '">' +
        (signatureRequired ? 'Enforced' : 'Optional') +
        '</span></div><p class="mu">' +
        (signatureRequired
          ? 'Only DLLs trusted by Windows Authenticode validation are eligible to load. Verification uses cached trust and revocation data; missing or invalid trust data fails closed. Installed DLLs that do not pass verification are blocked from loading, and safe unload is attempted. If a DLL may still be mapped, cleanup is persisted and retried during later background scans; 00047 never forces unsafe unmapping.'
          : 'Off by default for backwards compatibility. Turning this on blocks unsigned, untrusted, revoked, or unverifiable DLLs. Verification uses cached Windows trust and revocation data only and does not contact a network service.') +
        '</p><small class="mu">Signatures do not sandbox native code or guarantee that a signed DLL is safe.</small></div>' +
        '<button class="btn ' + (signatureRequired ? '' : 'primary') +
        '" data-native-action="signature-policy" data-signature-required="' + String(!signatureRequired) + '">' +
        (signatureRequired ? 'Disable enforcement' : 'Require trusted signatures') + '</button>';
      let targets = '';
      if (info.targets.length) {
        targets = '<div class="card glass native-target-card"><div class="sec"><h3>Supported target processes</h3><span class="mu">' + info.targets.length + ' found</span></div><div class="native-target-list">' +
          info.targets.map((target) =>
            '<div class="native-target"><div><b>' + esc(target.executable) + '</b><small class="mu">PID ' + target.pid + ' · ' + esc(target.architecture) + '</small></div><span class="plugin-status ' +
            (target.eligible ? 'native-eligible' : 'native-ineligible') + '">' + esc(target.detail) + '</span></div>'
          ).join('') + '</div></div>';
      } else {
        targets = '<div class="card glass native-target-card"><div class="sec"><h3>Supported target processes</h3><span class="mu">None detected</span></div><p class="mu">No supported shell process was detected right now. Explorer and the Start menu host may start later; enabled mods are retried automatically while 00047 is running.</p><small class="mu">Allow-list: explorer.exe, StartMenuExperienceHost.exe, ShellExperienceHost.exe, SearchHost.exe</small></div>';
      }

      let mods = '';
      if (info.mods.length) {
        mods = '<div class="native-mod-grid">' + info.mods.map((mod) =>
          '<article class="card glass native-mod-card">' +
          '<div class="native-mod-head"><div><h3>' + esc(mod.name) + ' <small class="mu">v' + esc(mod.version) + '</small></h3><p class="mu">' + esc(mod.description || 'No description provided.') + '</p><code>' + esc(mod.id) + '</code></div><span class="plugin-status ' +
          (mod.enabled ? 'native-eligible' : '') + '">' + (mod.enabled ? 'Enabled' : 'Disabled') + '</span></div>' +
          '<div class="native-mod-meta"><div><small class="mu">Architecture</small><b>' + esc(mod.architecture) + '</b></div><div><small class="mu">Module</small><code>' + esc(mod.module) + '</code></div><div><small class="mu">Targets</small><span>' + mod.targets.map(esc).join(', ') + '</span></div></div>' +
          '<div class="native-mod-status"><small class="mu">Runtime status</small><span>' + esc(mod.status) + '</span></div>' +
          (Array.isArray(mod.targetStatuses) && mod.targetStatuses.length
            ? '<div class="native-target-lifecycle"><small class="mu">Per-process lifecycle</small>' +
              mod.targetStatuses.map((target) =>
                '<div class="native-target-lifecycle-row"><div><b>' + esc(target.executable) + '</b><small class="mu">PID ' + esc(target.pid) + '</small></div><span class="native-lifecycle-state native-state-' + esc(target.state) + '">' + esc(target.state) + '</span><p>' + esc(target.detail) + '</p></div>'
              ).join('') + '</div>'
            : '') +
          '<div class="native-mod-actions">' +
          (mod.enabled
            ? '<button class="btn danger" data-native-action="disable" data-mod-id="' + esc(mod.id) + '">Disable and unload</button>'
            : '<button class="btn primary" data-native-action="enable" data-mod-id="' + esc(mod.id) + '">Enable mod</button>') +
          '<button class="btn" data-native-action="uninstall" data-mod-id="' + esc(mod.id) + '">Remove</button>' +
          '<button class="btn" data-native-action="settings" data-mod-id="' + esc(mod.id) + '">Settings</button>' +
          '<button class="btn" data-native-action="logs" data-mod-id="' + esc(mod.id) + '">View logs</button></div></article>'
        ).join('') + '</div>';
      } else {
        mods = '<div class="card glass plugin-empty"><div class="plugin-icon">+</div><b>No native mods installed</b><p class="mu">Install an x64 native-mod ZIP containing manifest.json and the compiled DLL.</p><button class="btn primary" data-native-action="install">Install your first native mod</button></div>';
      }
      $('#native-targets').innerHTML = targets;
      $('#native-mod-list').innerHTML = '<div class="sec native-section-head"><h3>Installed native mods</h3><span class="mu">' + info.mods.length + ' installed</span></div>' + mods;
      const signaturePending = new Set(info.pendingSignatureUnloads || []);
      const safeUnloadPending = new Set([
        ...(info.pendingSignatureUnloads || []),
        ...(info.pendingSafeUnloads || [])
      ]);
      const safeUnloadWarnings = Array.from(safeUnloadPending).map((id) =>
        (signaturePending.has(id) ? 'Trusted-signature cleanup' : 'Safe-unload cleanup') +
        ' for "' + id + '" is still pending. 00047 retries cleanup in the background; do not re-enable, update, or remove this package until unload is confirmed. If the warning persists, restart the affected shell process.'
      );
      const nativeWarnings = (info.errors || []).concat(safeUnloadWarnings);
      $('#native-mod-errors').innerHTML = nativeWarnings.length
        ? '<div class="plugin-errors"><b>Native mod warnings</b><ul>' + nativeWarnings.map((error) => '<li>' + esc(error) + '</li>').join('') + '</ul></div>'
        : '';
      bindActions(host);
    } catch (error) {
      $('#native-mod-list').innerHTML = '<p class="er">Could not load native mods: ' + esc(error) + '</p><button class="btn" data-native-action="refresh">Retry</button>';
      bindActions(host);
    } finally {
      refreshInFlight = false;
      // Poll only while the native-mod view is active. The read-only refresh
      // command can now observe loading/stopping states while the background
      // monitor or a user action is working on a target process.
      if (getView() === 'native-mods') {
        lifecycleRefreshTimer = setTimeout(() => {
          lifecycleRefreshTimer = null;
          void refreshData();
        }, 1200);
      }
    }
  }

  async function handleAction(button) {
    const action = button.dataset.nativeAction;
    const id = button.dataset.modId;
    try {
      if (action === 'folder') {
        await invoke('open_native_mods_folder');
      } else if (action === 'refresh') {
        await refreshData();
        return;
      } else if (action === 'signature-policy') {
        const required = button.dataset.signatureRequired === 'true';
        const answer = await modal({
          title: required ? 'Require trusted signatures?' : 'Disable signature enforcement?',
          body: required
            ? '00047 will validate the Authenticode signature and cached certificate trust/revocation status before loading native DLLs. Installed mods that do not pass will be removed from enabled intent and safe unload will be attempted. If the host cannot confirm unload safety, the DLL remains mapped and cleanup is persisted for retry during later background scans; restart that shell process rather than forcing an unload if it remains stuck. No network retrieval is used for verification.'
            : 'This allows unsigned or untrusted native DLLs to be enabled again. Native DLLs remain unsandboxed and can crash or hang Explorer, Start, or search. Only disable enforcement when you understand and accept that risk.',
          buttons: [['cancel', 'Cancel'], ['go', required ? 'Enable enforcement' : 'Disable enforcement', 'danger']]
        });
        if (answer !== 'go') return;
        const status = await invoke('native_mod_signature_policy_set', { required });
        toast(String(status));
      } else if (action === 'build') {
        await buildSourceProject();
      } else if (action === 'install') {
        await installPackage();
      } else if (action === 'enable') {
        const answer = await modal({
          title: 'Enable native mod?',
          body: 'This will load the native DLL for "' + id + '" into its declared Windows shell processes and call WmodInitializeV1. Native DLLs are not sandboxed. A bad mod can crash or hang Explorer, Start, or search. Continue only if you trust this code.',
          buttons: [['cancel', 'Cancel'], ['go', 'Enable mod', 'danger']]
        });
        if (answer !== 'go') return;
        const status = await invoke('native_mod_enable', { id });
        toast(String(status));
      } else if (action === 'disable') {
        const answer = await modal({
          title: 'Disable native mod?',
          body: '00047 will call WmodShutdownV1 and attempt to unload "' + id + '" from supported processes where it is loaded. A hung or inaccessible target may require restarting that shell component.',
          buttons: [['cancel', 'Cancel'], ['go', 'Disable and unload', 'danger']]
        });
        if (answer !== 'go') return;
        const status = await invoke('native_mod_disable', { id });
        toast(String(status));
      } else if (action === 'settings-browse') {
        const format = button.dataset.settingBrowseFormat;
        const picked = await invoke('native_mod_pick_setting_path', { folder: format === 'folderPath' });
        if (picked == null) return;
        const field = button.closest('.native-setting-field');
        const input = field?.querySelector('[data-setting-id]');
        if (input) {
          input.value = picked;
          input.dispatchEvent(new Event('input', { bubbles: true }));
          input.dispatchEvent(new Event('change', { bubbles: true }));
        }
        return;
      } else if (action === 'array-add') {
        const panel = button.closest('.native-settings-panel');
        const field = Array.from(panel?.querySelectorAll('[data-array-setting-id]') || [])
          .find(item => item.dataset.arraySettingId === button.dataset.arrayActionSettingId);
        const template = field?.querySelector('template[data-array-template]');
        const list = field?.querySelector('.native-array-list');
        if (!template || !list) return;
        list.append(template.content.cloneNode(true));
        bindActions(list);
        updateSettingVisibility(button.closest('.native-settings-form'));
        return;
      } else if (action === 'array-remove') {
        const form = button.closest('.native-settings-form');
        button.closest('.native-array-item')?.remove();
        updateSettingVisibility(form);
        return;
      } else if (action === 'settings') {
        await toggleSettings(button, id);
        return;
      } else if (action === 'settings-close') {
        button.closest('.native-settings-panel')?.remove();
        return;
      } else if (action === 'settings-save') {
        await saveSettings(button, id);
        return;
      } else if (action === 'logs') {
        await toggleLogs(button, id);
        return;
      } else if (action === 'logs-close') {
        button.closest('.native-log-panel')?.remove();
        return;
      } else if (action === 'uninstall') {
        const answer = await modal({
          title: 'Remove native mod?',
          body: 'This removes the installed files for "' + id + '". If enabled, 00047 first tries to disable and unload it from supported target processes.',
          buttons: [['cancel', 'Cancel'], ['go', 'Remove', 'danger']]
        });
        if (answer !== 'go') return;
        await invoke('native_mod_uninstall', { id });
        toast('Native mod removed');
      }
    } catch (error) {
      toast('Native mod operation failed: ' + error, 'err');
    }
    if (getView() === 'native-mods') await renderNativeMods();
  }

  function renderArrayItem(setting, value) {
    const options = Array.isArray(setting.options) ? setting.options : [];
    const isSelect = setting.dynamicSelect === true || options.length > 0;
    let control = '';
    if (isSelect) {
      control = '<select data-array-item="true" data-array-item-type="select">' +
        options.map(option => '<option value="' + esc(option.value) + '" ' +
          (String(value) === String(option.value) ? 'selected' : '') + '>' + esc(option.label) + '</option>').join('') +
        '</select>';
    } else {
      const maxLength = setting.maxLength == null ? 1024 : setting.maxLength;
      control = '<input type="text" data-array-item="true" data-array-item-type="string" maxlength="' +
        esc(String(maxLength)) + '" value="' + esc(String(value == null ? '' : value)) + '">';
    }
    return '<div class="native-array-item">' + control +
      '<button class="btn" type="button" data-native-action="array-remove" data-array-action-setting-id="' +
      esc(setting.id) + '">Remove</button></div>';
  }

  function renderSettingField(setting, value) {
    const id = esc(setting.id);
    const label = esc(setting.label);
    const description = setting.description ? '<small class="mu">' + esc(setting.description) + '</small>' : '';
    const kind = setting.type;
    const format = setting.format;
    let control = '';
    if (kind === 'string' && (format === 'colorRgb' || format === 'colorArgb')) {
      const text = String(value == null ? '' : value);
      const argb = format === 'colorArgb';
      const valid = argb ? /^#[0-9a-fA-F]{8}$/.test(text) : /^#[0-9a-fA-F]{6}$/.test(text);
      const alpha = argb && valid ? text.slice(1, 3).toUpperCase() : 'FF';
      const picker = valid ? (argb ? '#' + text.slice(3) : text) : '#000000';
      control = '<div class="native-color-field"><input type="color" data-setting-id="' + id +
        '" data-setting-type="formatted-color" data-setting-format="' + format + '" data-argb-alpha="' + alpha +
        '" value="' + esc(picker) + '">' +
        (argb ? '<span class="native-color-alpha">Alpha <input type="range" min="0" max="255" step="1" data-color-alpha-for="' +
          id + '" value="' + parseInt(alpha, 16) + '"></span>' : '') + '</div>';
    } else if (format === 'slider' && setting.min != null && setting.max != null &&
        ['string', 'integer', 'number'].includes(kind)) {
      const step = setting.step != null ? String(setting.step) : (kind === 'integer' ? '1' : 'any');
      const settingType = kind === 'string' ? 'slider-string' : kind;
      control = '<div class="native-slider-field"><input type="range" data-setting-id="' + id +
        '" data-setting-type="' + settingType + '" min="' + esc(String(setting.min)) +
        '" max="' + esc(String(setting.max)) + '" step="' + esc(step) + '" value="' + esc(String(value)) +
        '"><output data-setting-output-id="' + id + '">' + esc(String(value)) + '</output></div>';
    } else if (kind === 'string' && (format === 'filePath' || format === 'folderPath')) {
      const maxLength = setting.maxLength == null ? 1024 : setting.maxLength;
      control = '<div class="native-path-field"><input type="text" data-setting-id="' + id +
        '" data-setting-type="string" maxlength="' + esc(String(maxLength)) + '" value="' +
        esc(String(value == null ? '' : value)) + '" required><button class="btn" type="button" data-native-action="settings-browse" data-setting-browse-for="' +
        id + '" data-setting-browse-format="' + format + '">Browse…</button></div>';
    } else if (kind === 'stringArray') {
      const items = Array.isArray(value) ? value : [];
      const itemMarkup = items.map(item => renderArrayItem(setting, item)).join('');
      control = '<div class="native-array-field" data-array-setting-id="' + id + '">' +
        '<div class="native-array-list">' + itemMarkup + '</div>' +
        '<template data-array-template>' + renderArrayItem(setting, '') + '</template>' +
        '<button class="btn" type="button" data-native-action="array-add" data-array-action-setting-id="' + id + '">Add value</button></div>';
    } else if (kind === 'boolean') {
      control = '<input type="checkbox" data-setting-id="' + id + '" data-setting-type="boolean" ' + (value === true ? 'checked' : '') + '>';
    } else if (kind === 'select' || (kind === 'string' && (setting.dynamicSelect === true || (Array.isArray(setting.options) && setting.options.length > 0)))) {
      control = '<select data-setting-id="' + id + '" data-setting-type="select">' +
        (setting.options || []).map(option => '<option value="' + esc(option.value) + '" ' + (String(value) === String(option.value) ? 'selected' : '') + '>' + esc(option.label) + '</option>').join('') +
        '</select>';
    } else if (kind === 'number' || kind === 'integer') {
      const min = setting.min == null ? '' : ' min="' + esc(String(setting.min)) + '"';
      const max = setting.max == null ? '' : ' max="' + esc(String(setting.max)) + '"';
      const step = setting.step != null ? String(setting.step) : (kind === 'integer' ? '1' : 'any');
      control = '<input type="number" data-setting-id="' + id + '" data-setting-type="' + kind + '"' + min + max + ' step="' + esc(step) + '" value="' + esc(String(value)) + '" required>';
    } else {
      const maxLength = setting.maxLength == null ? 1024 : setting.maxLength;
      control = '<input type="text" data-setting-id="' + id + '" data-setting-type="string" maxlength="' + esc(String(maxLength)) + '" value="' + esc(String(value == null ? '' : value)) + '" required>';
    }
    const wrapper = kind === 'stringArray' ? 'div' : 'label';
    const showIf = esc(JSON.stringify(setting.showIf || {}));
    const hideIf = esc(JSON.stringify(setting.hideIf || {}));
    return '<' + wrapper + ' class="native-setting-field" data-setting-field-id="' + id +
      '" data-show-if="' + showIf + '" data-hide-if="' + hideIf + '"><span><b>' + label + '</b>' + description +
      '</span>' + control + '</' + wrapper + '>';
  }

  function collectSettingsDraft(form) {
    const values = {};
    if (!form) return values;
    const alphaById = new Map();
    for (const alpha of form.querySelectorAll('[data-color-alpha-for]')) {
      const parsed = Number.parseInt(alpha.value, 10);
      alphaById.set(alpha.dataset.colorAlphaFor,
        Number.isFinite(parsed) ? parsed.toString(16).padStart(2, '0').toUpperCase() : 'FF');
    }
    for (const control of form.querySelectorAll('[data-setting-id]')) {
      const key = control.dataset.settingId;
      const kind = control.dataset.settingType;
      if (kind === 'boolean') values[key] = control.checked;
      else if (kind === 'integer') values[key] = Number.parseInt(control.value, 10);
      else if (kind === 'number') values[key] = Number(control.value);
      else if (kind === 'formatted-color') {
        const color = control.value.toUpperCase();
        values[key] = control.dataset.settingFormat === 'colorArgb'
          ? '#' + (alphaById.get(key) || control.dataset.argbAlpha || 'FF') + color.slice(1)
          : color;
      } else if (kind === 'slider-string') values[key] = control.value;
      else values[key] = control.value;
    }
    for (const field of form.querySelectorAll('[data-array-setting-id]')) {
      values[field.dataset.arraySettingId] = Array.from(field.querySelectorAll('[data-array-item]')).map(item => item.value);
    }
    return values;
  }

  function conditionHolds(actual, accepted) {
    const expected = Array.isArray(accepted) ? accepted : [accepted];
    const equal = (left, right) => typeof left === typeof right && left === right;
    if (Array.isArray(actual)) return actual.some(value => expected.some(candidate => equal(value, candidate)));
    return expected.some(candidate => equal(actual, candidate));
  }

  function updateSettingOutputs(form) {
    if (!form) return;
    const controls = Array.from(form.querySelectorAll('[data-setting-id]'));
    for (const output of form.querySelectorAll('[data-setting-output-id]')) {
      const control = controls.find(item => item.dataset.settingId === output.dataset.settingOutputId);
      if (control) output.textContent = control.value;
    }
  }

  function updateSettingVisibility(form) {
    if (!form) return;
    updateSettingOutputs(form);
    const fields = Array.from(form.querySelectorAll('[data-setting-field-id]'));
    const byId = new Map(fields.map(field => [field.dataset.settingFieldId, field]));
    const metadata = new Map();
    for (const field of fields) {
      const parse = value => {
        try {
          const result = JSON.parse(value || '{}');
          return result && typeof result === 'object' && !Array.isArray(result) ? result : {};
        } catch (_) { return {}; }
      };
      metadata.set(field.dataset.settingFieldId, {
        showIf: parse(field.dataset.showIf),
        hideIf: parse(field.dataset.hideIf)
      });
    }
    const draft = collectSettingsDraft(form);
    const memo = new Map();
    const visiting = new Set();
    const isShown = id => {
      if (memo.has(id)) return memo.get(id);
      if (visiting.has(id)) return true;
      const field = byId.get(id);
      if (!field) return false;
      visiting.add(id);
      const conditions = metadata.get(id);
      let shown = true;
      for (const [dependency, accepted] of Object.entries(conditions.showIf)) {
        if (!isShown(dependency) || !conditionHolds(draft[dependency], accepted)) { shown = false; break; }
      }
      if (shown) {
        for (const [dependency, accepted] of Object.entries(conditions.hideIf)) {
          if (!isShown(dependency) || conditionHolds(draft[dependency], accepted)) { shown = false; break; }
        }
      }
      visiting.delete(id);
      memo.set(id, shown);
      return shown;
    };
    for (const field of fields) {
      const shown = isShown(field.dataset.settingFieldId);
      field.hidden = !shown;
      field.style.display = shown ? '' : 'none';
      for (const control of field.querySelectorAll('[data-setting-id]')) {
        if (control.type === 'text' || control.type === 'number') control.required = shown;
      }
    }
  }

  async function toggleSettings(button, id) {
    const card = button.closest('.native-mod-card');
    if (!card) return;
    const existing = card.querySelector('.native-settings-panel');
    if (existing) { existing.remove(); return; }
    const data = await invoke('native_mod_settings_read', { id });
    const panel = document.createElement('div');
    panel.className = 'native-settings-panel';
    if (!data.settings.length) {
      panel.innerHTML = '<h4>Mod settings</h4><p class="mu">This mod does not declare settings in manifest.json. Add a "settings" array to expose editable values.</p><button class="btn" type="button" data-native-action="settings-close" data-mod-id="' + esc(id) + '">Close</button>';
    } else {
      panel.innerHTML = '<h4>Mod settings</h4><form class="native-settings-form">' +
        data.settings.map(setting => renderSettingField(setting, data.values[setting.id])).join('') +
        '<div class="native-settings-actions"><button class="btn primary" type="button" data-native-action="settings-save" data-mod-id="' + esc(id) + '">Save settings</button><button class="btn" type="button" data-native-action="settings-close" data-mod-id="' + esc(id) + '">Close</button></div>' +
        '<p class="mu">Values are saved to settings.json. A mod must read and reload this file for changes to affect its behavior.</p></form>';
    }
    card.append(panel);
    bindActions(card);
    const form = panel.querySelector('.native-settings-form');
    if (form) {
      form.addEventListener('input', () => updateSettingVisibility(form));
      form.addEventListener('change', () => updateSettingVisibility(form));
      updateSettingOutputs(form);
      updateSettingVisibility(form);
    }
  }

  async function saveSettings(button, id) {
    const panel = button.closest('.native-settings-panel');
    if (!panel) return;
    const form = panel.querySelector('.native-settings-form');
    const values = collectSettingsDraft(form);
    if (form && !form.reportValidity()) return;
    await invoke('native_mod_settings_write', { id, values });
    toast('Settings saved. The mod must reload settings.json to apply changes.');
  }

  async function toggleLogs(button, id) {
    const card = button.closest('.native-mod-card');
    if (!card) return;
    const existing = card.querySelector('.native-log-panel');
    if (existing) { existing.remove(); return; }
    const lines = await invoke('native_mod_logs_read', { id });
    const panel = document.createElement('div');
    panel.className = 'native-log-panel';
    panel.innerHTML = '<h4>Native mod log</h4>' +
      (lines.length ? '<pre>' + lines.map(line => esc(line)).join('\n') + '</pre>' :
        '<p class="mu">No mod.log output yet. Include the SDK helper and call wmod::Log(level, message) to write structured JSON Lines here.</p>') +
      '<button class="btn" type="button" data-native-action="logs-close" data-mod-id="' + esc(id) + '">Close</button>';
    card.append(panel);
    bindActions(card);
  }

  async function installPackage() {
    try {
      const preview = await invoke('native_mod_pack_preview');
      if (!preview) return;
      const targets = preview.targets.map(esc).join(', ');
      const exports = preview.exports.map(esc).join(', ');
      const answer = await htmlModal('Install native mod "' + preview.name + '"?',
        '<p><code>' + esc(preview.id) + '</code> · version ' + esc(preview.version) + '</p>' +
        '<p>' + esc(preview.description || '') + '</p>' +
        '<div class="native-preview-grid"><div><small class="mu">Architecture</small><b>' + esc(preview.architecture) + '</b></div><div><small class="mu">DLL</small><code>' + esc(preview.module) + '</code></div><div><small class="mu">Allowed targets</small><span>' + targets + '</span></div><div><small class="mu">Required exports</small><code>' + exports + '</code></div></div>' +
        '<p class="er">Installing stores the package; enabling later executes its DLL inside each selected shell process. Review and trust the source before enabling.</p>',
        [['cancel', 'Cancel'], ['go', 'Install package', 'primary']]);
      if (answer !== 'go') return;
      await invoke('native_mod_pack_install', { path: preview.path });
      toast('Native mod package installed (disabled by default)');
    } catch (error) {
      toast('Native mod install failed: ' + error, 'err');
    }
  }

  async function buildSourceProject() {
    const answer = await modal({
      title: 'Build and install from source?',
      body: 'Choose a native-mod project folder containing manifest.json and either CMakeLists.txt or Cargo.toml. CMake projects use the bundled SDK and Visual Studio C++ x64 toolchain; Cargo projects build a Rust x64 cdylib and require a current Cargo.lock. Build files and scripts execute with your Windows user permissions, so only build source you trust. The package is validated and installed disabled by default. When trusted-signature enforcement is enabled, the resulting DLL must also have a valid trusted Authenticode signature.',
      buttons: [['cancel', 'Cancel'], ['go', 'Choose source folder', 'primary']]
    });
    if (answer !== 'go') return;
    try {
      const result = await invoke('native_mod_build_source_and_install');
      if (!result) return;
      toast(result.message || ('Built and installed ' + result.id + ' (disabled by default)'));
      const log = String(result.buildOutput || '').slice(-40000);
      await htmlModal('Native mod build complete',
        '<p>Installed <code>' + esc(result.id) + '</code>. It is disabled until you explicitly enable it.</p>' +
        '<p class="mu">Build log (last 40 KB):</p><pre class="native-build-log">' + esc(log) + '</pre>',
        [['close', 'Close', 'primary']]);
      await refreshData();
    } catch (error) {
      toast('Native mod build failed: ' + error, 'err');
    }
  }

  return renderNativeMods;
}
