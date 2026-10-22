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
    host.innerHTML = '<div class="plugins-head"><div><h2>Native Windows Mods</h2><p class="mu">Manage native x64 DLL modules for supported Windows shell processes.</p></div><div class="plugins-actions"><button class="btn" data-native-action="folder">Open native mods folder</button><button class="btn" data-native-action="refresh">Refresh targets</button><button class="btn primary" data-native-action="install">+ Install native mod ZIP...</button></div></div>' +
      '<div class="native-warning card glass"><b>High-impact native code</b><p class="mu">A native mod runs inside Explorer or another listed shell process with that process permissions. A faulty or hostile DLL can hang or crash it. Only enable source you trust. The loader accepts x64 DLLs and a fixed allow-list of Windows shell processes, not arbitrary targets.</p></div>' +
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
      $('#native-mod-errors').innerHTML = info.errors.length
        ? '<div class="plugin-errors"><b>Native mod warnings</b><ul>' + info.errors.map((error) => '<li>' + esc(error) + '</li>').join('') + '</ul></div>'
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
      } else if (action === 'array-add') {
        const panel = button.closest('.native-settings-panel');
        const field = Array.from(panel?.querySelectorAll('[data-array-setting-id]') || [])
          .find(item => item.dataset.arraySettingId === button.dataset.arrayActionSettingId);
        const template = field?.querySelector('template[data-array-template]');
        const list = field?.querySelector('.native-array-list');
        if (!template || !list) return;
        list.append(template.content.cloneNode(true));
        bindActions(list);
        return;
      } else if (action === 'array-remove') {
        button.closest('.native-array-item')?.remove();
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
    let control = '';
    if (kind === 'stringArray') {
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
    return '<' + wrapper + ' class="native-setting-field"><span><b>' + label + '</b>' + description +
      '</span>' + control + '</' + wrapper + '>';
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
  }

  async function saveSettings(button, id) {
    const panel = button.closest('.native-settings-panel');
    if (!panel) return;
    const form = panel.querySelector('.native-settings-form');
    const values = {};
    for (const control of panel.querySelectorAll('[data-setting-id]')) {
      const key = control.dataset.settingId;
      const kind = control.dataset.settingType;
      if (kind === 'boolean') values[key] = control.checked;
      else if (kind === 'integer') values[key] = Number.parseInt(control.value, 10);
      else if (kind === 'number') values[key] = Number(control.value);
      else values[key] = control.value;
    }
    for (const field of panel.querySelectorAll('[data-array-setting-id]')) {
      const key = field.dataset.arraySettingId;
      values[key] = Array.from(field.querySelectorAll('[data-array-item]')).map(item => item.value);
    }
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

  return renderNativeMods;
}
