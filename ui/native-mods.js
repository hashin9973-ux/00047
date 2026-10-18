export function createNativeModsPage({ invoke, esc, toast, modal, htmlModal, getView }) {
  const $ = (selector) => document.querySelector(selector);

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
          '<div class="native-mod-actions">' +
          (mod.enabled
            ? '<button class="btn danger" data-native-action="disable" data-mod-id="' + esc(mod.id) + '">Disable and unload</button>'
            : '<button class="btn primary" data-native-action="enable" data-mod-id="' + esc(mod.id) + '">Enable mod</button>') +
          '<button class="btn" data-native-action="uninstall" data-mod-id="' + esc(mod.id) + '">Remove</button></div></article>'
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
