// semilla review UI. This script has no build step and loads no remote code.
// It uses the global libraries that the page loads: cytoscape (with the elk layout) and mermaid.
'use strict';

(function () {
  // ---------------------------------------------------------------- helpers

  const $ = (selector, root) => (root || document).querySelector(selector);
  const enc = encodeURIComponent;

  // Makes a DOM element. Text children become text nodes, so the data cannot inject HTML.
  function h(tag, attrs, ...children) {
    const node = document.createElement(tag);
    for (const [key, value] of Object.entries(attrs || {})) {
      if (value === undefined || value === null || value === false) continue;
      if (key === 'class') node.className = value;
      else if (key.startsWith('on') && typeof value === 'function') node.addEventListener(key.slice(2), value);
      else if (key === 'dataset') Object.assign(node.dataset, value);
      else if (value === true) node.setAttribute(key, '');
      else node.setAttribute(key, key === 'title' ? show(value) : String(value));
    }
    append(node, children);
    return node;
  }

  // Text of the agent or of the repository can hold hidden characters: control characters,
  // bidirectional controls, zero-width characters (batch 5k, A2). The UI shows each one as an
  // escape. A line break stays: the browser shows it as a space or, in a code block, as a line.
  const HIDDEN = /[\u0000-\u0009\u000b-\u001f\u007f-\u009f\u061c\u200b-\u200f\u202a-\u202e\u2028\u2029\u2060\u2066-\u2069\ufeff]/g;
  const escapeOf = (c) => (c === '\n' ? '\\n' : c === '\r' ? '\\r' : c === '\t' ? '\\t' : '\\u{' + c.charCodeAt(0).toString(16) + '}');
  function show(text) {
    return String(text).replace(HIDDEN, escapeOf);
  }
  // The words of the agent, in quote marks (A3): the text cannot close its quote, and a line
  // break shows as an escape.
  function quoted(text) {
    return '"' + show(String(text).replace(/["\\]/g, '\\$&')).replace(/\n/g, '\\n') + '"';
  }

  function append(node, children) {
    for (const child of children.flat(Infinity)) {
      if (child === undefined || child === null || child === false) continue;
      node.appendChild(child instanceof Node ? child : document.createTextNode(show(child)));
    }
    return node;
  }

  function clear(node) {
    while (node.firstChild) node.removeChild(node.firstChild);
    return node;
  }

  async function api(path, options) {
    const response = await fetch(path, options);
    let body = null;
    try {
      body = await response.json();
    } catch (_) {
      body = null;
    }
    if (!response.ok) {
      const error = new Error((body && body.error) || response.statusText);
      error.status = response.status;
      throw error;
    }
    return body;
  }

  let toastTimer = null;
  function toast(text) {
    const node = $('#toast');
    node.textContent = show(text);
    node.hidden = false;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => { node.hidden = true; }, 3500);
  }

  function fail(error) {
    console.error(error);
    toast('Error: ' + (error && error.message ? error.message : String(error)));
  }

  const MARKERS = { same: '≡', added: '+', removed: '−', changed: '~', moved: '→', extracted: '⤴', uncovered: '✗' };
  const STATUS_ICONS = { verified: '✓', unproved: '?', refuted: '✗', empty: '∅', malformed: '!', pending: '…', not_checked: '–', no_result: '·' };
  const CLASS_LABELS = { neutral: 'neutral', logic: 'logic', control_flow: 'ctrl flow', other: 'other', not_checked: 'not checked' };
  // The class of a fact in a text line. A step with no check gives `not checked` (design 28).
  // The title of a build code change of a step (design 28.3).
  const BUILD_CODE_SECTION = 'build code changed in this step; read this change first';
  const factClass = (fact) => (fact.class === 'not_checked' ? 'not checked' : fact.class || 'class after phase 2');

  function marker(status) {
    return h('span', { class: 'marker m ' + (status || 'same'), title: status || '' }, MARKERS[status] || '·');
  }

  // Design 28, I6: `buildCode` is the step whose build code change the proof of the class
  // used. The chip then has the mark.
  function classChip(cls, buildCode) {
    if (!cls) return null;
    return h('span', { class: 'chip cls-' + cls }, (CLASS_LABELS[cls] || cls) + (buildCode != null ? ' (with the build code of step ' + buildCode + ')' : ''));
  }

  // Shows `⟦old⟧→⟦new⟧` parts as a deletion and an insertion.
  function diffText(text) {
    const out = [];
    const pattern = /⟦([^⟧]*)⟧→⟦([^⟧]*)⟧/g;
    let last = 0;
    let match;
    while ((match = pattern.exec(text)) !== null) {
      if (match.index > last) out.push(text.slice(last, match.index));
      if (match[1]) out.push(h('del', { class: 'd' }, match[1]));
      out.push('→');
      if (match[2]) out.push(h('ins', { class: 'd' }, match[2]));
      last = pattern.lastIndex;
    }
    if (last < text.length) out.push(text.slice(last));
    return out;
  }

  function shortId(id) {
    const item = String(id).split('/')[0];
    const parts = item.split('::');
    return parts.length > 2 ? parts.slice(-2).join('::') + String(id).slice(item.length) : String(id);
  }

  function cssVar(name) {
    return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  }

  // ---------------------------------------------------------------- state

  const state = {
    view: 'changeset',
    changeset: null,
    openGroups: null,
    openSubgroups: {},
    tree: null,
    graph: null,
    flowStack: [],
    flowIndex: -1,
    flowCurrent: null,
    cy: null,
    mapSelected: null,
    mapFocus: null,
  };

  // ---------------------------------------------------------------- theme

  const THEMES = ['auto', 'light', 'dark'];

  function storedTheme() {
    try {
      return localStorage.getItem('semilla-theme') || 'auto';
    } catch (_) {
      return 'auto';
    }
  }

  function applyTheme(theme) {
    if (theme === 'auto') delete document.documentElement.dataset.theme;
    else document.documentElement.dataset.theme = theme;
    $('#theme').title = 'Theme: ' + theme + ' (click to change)';
    try {
      localStorage.setItem('semilla-theme', theme);
    } catch (_) {
      // The browser does not keep the theme. That is not a problem.
    }
  }

  function isDark() {
    const theme = document.documentElement.dataset.theme;
    if (theme) return theme === 'dark';
    return window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches;
  }

  function onThemeChange() {
    if (state.flowCurrent) renderFlow(state.flowCurrent);
    if (state.cy) styleMap();
  }

  // ---------------------------------------------------------------- routing

  function showView(view) {
    state.view = view;
    for (const name of ['changeset', 'tree', 'flow', 'map']) {
      $('#view-' + name).hidden = name !== view;
    }
    for (const link of document.querySelectorAll('.tabs a')) {
      link.classList.toggle('active', link.dataset.view === view);
    }
    if (view === 'tree' && !state.tree) loadTree().catch(fail);
    if (view === 'map') openMap().catch(fail);
    if (view === 'flow' && !state.flowCurrent) {
      clear($('#flow-graph')).appendChild(
        h('p', { class: 'empty' }, 'Select a function in the change set, the change tree, the map, or the search to see its flow view.'),
      );
    }
  }

  function route() {
    const hash = decodeURIComponent(location.hash.replace(/^#\/?/, ''));
    const [view, ...rest] = hash.split('/');
    const arg = rest.join('/');
    if (view === 'flow' && arg) {
      showView('flow');
      syncFlowHistory(arg);
      loadFlow(arg).catch(fail);
    } else if (view === 'node' && arg) {
      openPanel(arg);
    } else if (['changeset', 'tree', 'flow', 'map'].includes(view)) {
      showView(view);
    } else {
      showView('changeset');
    }
  }

  // ---------------------------------------------------------------- change-set view

  async function loadChangeset() {
    state.changeset = await api('/api/changeset');
    renderChangeset();
  }

  // The steps of a group for its title (C11): the groups are sorted by risk, so the title gives
  // the order of the work, for example "steps 4–5".
  function stepsLabel(steps) {
    const sorted = [...(steps || [])].sort((a, b) => a - b);
    if (!sorted.length) return 'no steps';
    if (sorted.length === 1) return 'step ' + sorted[0];
    const first = sorted[0], last = sorted[sorted.length - 1];
    if (last - first + 1 === sorted.length) return 'steps ' + first + '–' + last;
    return 'steps ' + sorted.join(', ');
  }

  function riskDots(level) {
    const dots = [];
    for (let i = 0; i < 3; i++) dots.push(h('span', { class: i < level ? 'on' : 'off' }, i < level ? '●' : '○'));
    return dots;
  }

  function signed(value) {
    if (value === null || value === undefined) return null;
    if (value > 0) return '+' + value;
    if (value < 0) return '−' + Math.abs(value);
    return '±0';
  }

  function reviewBox(target, checked, label) {
    const input = h('input', { type: 'checkbox', checked: checked || undefined, 'aria-label': 'Reviewed: ' + target });
    input.addEventListener('click', (event) => event.stopPropagation());
    input.addEventListener('change', () => {
      setReviewed(target, input.checked).catch((error) => {
        input.checked = !input.checked;
        fail(error);
      });
    });
    return h('label', { class: 'review-box', onclick: (event) => event.stopPropagation() }, input, label || 'reviewed');
  }

  async function setReviewed(target, reviewed) {
    await api('/api/review/' + enc(target), { method: reviewed ? 'POST' : 'DELETE' });
    await loadChangeset();
    if (!$('#panel').hidden && $('#panel').dataset.node) openPanel($('#panel').dataset.node, true);
  }

  function renderChangeset() {
    const cs = state.changeset;
    const root = clear($('#view-changeset'));
    if (!cs) return;
    const c = cs.counts;
    const facts = [
      h('b', null, cs.group_count), ' groups · ',
      h('b', null, cs.step_count), ' steps · ',
      h('b', null, c.claims), ' claims · ',
      h('b', null, cs.fact_counts.total), ' facts: ',
      h('b', null, cs.fact_counts.neutral), ' proved neutral', cs.fact_counts.marked_text || '', ', ',
      h('b', null, cs.fact_counts.read), ' to read (',
      h('b', null, cs.fact_counts.spotlight), ' with a spotlight)',
      cs.fact_counts.not_checked ? ', ' + cs.fact_counts.not_checked + ' not checked' : '', ' · API: ',
      h('b', null, cs.api_level),
    ];
    if (cs.panic_delta !== null && cs.panic_delta !== undefined) facts.push(' · panics ', h('b', null, signed(cs.panic_delta)));
    root.appendChild(
      h('div', { class: 'cs-head' },
        h('h1', null, 'Change set'),
        h('span', { class: 'chip ' + (cs.accepted ? 'ok' : cs.not_complete ? 'warn' : 'bad') }, cs.accepted ? 'accepted' : cs.not_complete ? 'not complete' : 'rejected'),
        h('span', { class: 'facts' }, facts),
        h('span', { class: 'facts' },
          c.verified, ' verified', c.marked_text || '',
          c.partial ? ' · ' + c.partial + ' partial' + (c.partial_marked_text || '') : '',
          c.unproved ? ' · ' + c.unproved + ' unproved' : '',
          c.refuted ? ' · ' + c.refuted + ' refuted' : '',
          c.empty ? ' · ' + c.empty + ' empty' : '',
          c.malformed ? ' · ' + c.malformed + ' malformed' : '',
          c.pending ? ' · ' + c.pending + ' pending' : '',
          c.not_checked ? ' · ' + c.not_checked + ' not checked' : '',
          c.uncovered ? ' · ' + c.uncovered + ' uncovered' : ''),
        h('span', { class: 'progress' }, c.reviewed + ' / ' + c.claims + ' claims reviewed'),
      ),
    );
    // A3: the reviewer must know which text is not a result.
    root.appendChild(h('p', { class: 'facts agent-text-note' }, 'Text in quote marks is the text of the agent. Semilla does not check it.'));
    const messages = [];
    // The base commit and its Semilla check come first.
    for (const text of cs.notices || []) messages.push(h('li', { class: text.startsWith('!') ? 'warning' : 'notice' }, text));
    for (const text of cs.organization) messages.push(h('li', { class: 'error' }, '✗ ', text));
    for (const text of cs.warnings) messages.push(h('li', { class: 'warning' }, '⚠ ', text));
    if (messages.length) root.appendChild(h('ul', { class: 'messages' }, messages));
    if (cs.lines_text) root.appendChild(h('p', { class: 'facts lines' }, cs.lines_text));
    if (cs.facts && cs.facts.length) {
      // The derived facts (design 22.5): Semilla computed them from the diff; no claim exists.
      const items = cs.facts.map((fact) => {
        const parts = ['step ' + fact.step + ': ' + fact.id + ' derived [' + factClass(fact) + ']' + (fact.mark || '') + ': ' + fact.description];
        for (const group of fact.sites || []) if (!fact.description.startsWith(group.edit + ' in ')) parts.push(group.sites.length + ' site(s): ' + group.edit);
        if (fact.affected && fact.affected.length) parts.push(fact.affected.length + ' affected: ' + fact.affected.join(', '));
        if (fact.note) parts.push('note: ' + fact.note + (fact.part_mark || ''));
        if (fact.spotlight_why) parts.push('spotlight: ' + fact.spotlight_why + '; reason: ' + (fact.reason ? quoted(fact.reason) : 'MISSING'));
        // A build code change says so (design 28.3). The server puts it first in its step.
        const first = fact.build_code_change ? h('b', null, BUILD_CODE_SECTION + ': ') : null;
        return h('li', { class: fact.spotlight ? 'warning fact' : 'fact' }, '◆ ', first, parts.join('; '), fact.content ? h('pre', { class: 'diff' }, fact.content) : null);
      });
      root.appendChild(h('h3', {}, 'Derived facts'));
      root.appendChild(h('ul', { class: 'messages facts' }, items));
    }
    if (cs.pairs && cs.pairs.length) {
      const items = cs.pairs.map((pair) => {
        const decide = (decision) => async (event) => {
          event.stopPropagation();
          await api('/api/pair?old=' + enc(pair.old) + '&new=' + enc(pair.new) + '&decision=' + decision, { method: 'POST' });
          await loadChangeset();
        };
        const buttons = pair.reviewer
          ? [h('span', { class: 'chip ' + (pair.reviewer === 'accepted' ? 'bad' : 'ok') }, pair.reviewer), ' ', h('button', { onclick: decide('clear') }, 'undo')]
          : [h('button', { onclick: decide('accepted'), title: 'The agent must claim the pair as one changed item' }, 'accept warning'), ' ',
             h('button', { onclick: decide('dismissed'), title: 'Keep the removed item and the added item' }, 'dismiss')];
        return h('li', { class: 'warning pair' }, '⚠ pair: ', pair.text, ' ', ...buttons);
      });
      root.appendChild(h('ul', { class: 'messages pairs' }, items));
    }

    if (state.openGroups === null) {
      // At the start, open the first group that is not reviewed.
      const first = cs.groups.find((group) => !group.reviewed);
      state.openGroups = new Set(first ? [first.id] : []);
    }
    for (const group of cs.groups) root.appendChild(renderGroup(group));
    if (!cs.groups.length) root.appendChild(h('p', { class: 'empty' }, 'The change set has no groups.'));
  }

  function renderGroup(group) {
    const details = h('details', { class: 'group' + (group.reviewed ? ' reviewed' : ''), open: state.openGroups.has(group.id) || undefined });
    details.addEventListener('toggle', () => {
      if (details.open) state.openGroups.add(group.id);
      else state.openGroups.delete(group.id);
    });
    const strip = group.subgroups.map((sub) => sub.role_label + (sub.claims.length > 1 ? ' (' + sub.claims.length + ')' : '')).join(' · ');
    const right = [];
    if (group.all_neutral) right.push(h('span', { class: 'chip ok' }, '✓ all neutral' + (group.mark || '')));
    if (group.counts.uncovered) right.push(h('span', { class: 'chip bad' }, group.counts.uncovered + ' uncovered'));
    const failed = group.counts.claims - group.counts.verified - (group.counts.partial || 0);
    if (failed) right.push(h('span', { class: 'chip warn' }, failed + ' not verified'));
    if (group.panic_delta) right.push(h('span', { class: 'chip' }, 'panics ' + signed(group.panic_delta)));
    right.push(h('span', { class: 'chip' }, group.counts.reviewed + '/' + group.counts.claims));
    right.push(reviewBox(group.id, group.reviewed, 'group reviewed'));
    details.appendChild(
      h('summary', null,
        h('span', { class: 'twisty' }),
        h('span', { class: 'gid' }, group.id),
        h('span', { class: 'steps' }, stepsLabel(group.steps)),
        h('span', { class: 'kind' }, group.kind + (group.kind_mark || '')),
        h('span', { class: 'title' }, group.synthetic ? group.title : quoted(group.title)),
        h('span', { class: 'risk', title: 'risk: ' + (group.risk_reasons.join(', ') || 'none') }, 'risk ', riskDots(group.risk)),
        group.synthetic ? h('span', { class: 'chip warn' }, 'made by the UI') : null,
        h('span', { class: 'right' }, right),
        h('span', { class: 'sub-strip' }, strip),
      ),
    );
    const body = h('div', { class: 'body' });
    if (group.why) body.appendChild(h('p', { class: 'why' }, group.synthetic ? group.why : quoted(group.why)));
    const facts = ['steps ' + group.steps.join(', ')];
    if (group.depends_on.length) facts.push('comes after ' + group.depends_on.join(', '));
    if (group.risk_reasons.length) facts.push('risk: ' + group.risk_reasons.join(', '));
    body.appendChild(h('p', { class: 'why' }, facts.join(' · ')));
    if (group.split_hint) body.appendChild(h('p', { class: 'hint' }, '⚠ ', group.split_hint));
    // The derived facts of the steps of the group (design 22.5): as visible as its claims.
    // The build code changes come first, each with its diff (design 28.3).
    const allFacts = (state.changeset.facts || []).filter((fact) => group.steps.includes(fact.step));
    const factList = (title, facts) => {
      if (!facts.length) return;
      body.appendChild(h('h4', {}, title + ' (' + facts.length + ')'));
      body.appendChild(h('ul', { class: 'messages facts' }, facts.map((fact) => {
        const parts = ['step ' + fact.step + ': ' + fact.id + ' derived [' + factClass(fact) + ']' + (fact.mark || '') + ': ' + fact.description];
        for (const site of fact.sites || []) if (!fact.description.startsWith(site.edit + ' in ')) parts.push(site.sites.length + ' site(s): ' + site.edit);
        if (fact.affected && fact.affected.length) parts.push(fact.affected.length + ' affected: ' + fact.affected.join(', '));
        if (fact.note) parts.push('note: ' + fact.note + (fact.part_mark || ''));
        if (fact.spotlight_why) parts.push('spotlight: ' + fact.spotlight_why + '; reason: ' + (fact.reason ? quoted(fact.reason) : 'MISSING'));
        return h('li', { class: fact.spotlight ? 'warning fact' : 'fact' }, '◆ ', parts.join('; '), fact.content ? h('pre', { class: 'diff' }, fact.content) : null);
      })));
    };
    factList('Build code changes: read these changes first', allFacts.filter((fact) => fact.build_code_change));
    factList('Derived facts', allFacts.filter((fact) => !fact.build_code_change));
    if (group.uncovered.length) {
      body.appendChild(
        h('ul', { class: 'uncovered-list' },
          group.uncovered.map((item) =>
            h('li', null,
              h('b', null, '✗ uncovered '),
              item.at ? h('a', { class: 'mono', href: '#/node/' + enc(item.at), onclick: (event) => { event.preventDefault(); openPanel(item.at); } }, item.at) : item.file || '',
              ' (', item.view, ', step ', item.step, ') ',
              h('span', { class: 'mono' }, item.description),
              item.hint ? h('div', { class: 'hint-text' }, 'hint: ', item.hint) : null,
            ),
          ),
        ),
      );
    }
    for (const sub of group.subgroups) body.appendChild(renderSubgroup(sub));
    details.appendChild(body);
    return details;
  }

  function renderSubgroup(sub) {
    const known = Object.prototype.hasOwnProperty.call(state.openSubgroups, sub.id);
    const open = known ? state.openSubgroups[sub.id] : !sub.folded;
    const details = h('details', { class: 'subgroup' + (sub.reviewed ? ' reviewed' : ''), open: open || undefined });
    details.addEventListener('toggle', () => { state.openSubgroups[sub.id] = details.open; });
    const right = [];
    for (const badge of sub.badges) right.push(h('span', { class: 'chip' }, badge));
    if (sub.folded) right.push(h('span', { class: 'chip ok' }, '✓ proved' + (sub.mark || '')));
    if (sub.counts.unproved) right.push(h('span', { class: 'chip warn' }, sub.counts.unproved + ' unproved'));
    if (sub.counts.pending) right.push(h('span', { class: 'chip' }, sub.counts.pending + ' pending'));
    const bad = sub.counts.refuted + sub.counts.empty + sub.counts.malformed;
    if (bad) right.push(h('span', { class: 'chip bad' }, bad + ' failed'));
    right.push(reviewBox(sub.id, sub.reviewed));
    details.appendChild(
      h('summary', null,
        h('span', { class: 'twisty' }),
        h('span', { class: 'role' }, sub.role_label),
        h('span', { class: 'stitle' }, sub.title),
        sub.edited ? h('span', { class: 'chip edited', title: 'The agent changed the engine suggestion.' }, 'edited by agent') : null,
        sub.synthetic ? h('span', { class: 'chip warn' }, 'made by the UI') : null,
        sub.new_claims.length
          ? h('span', { class: 'chip new', title: 'The engine added these claims after the agent edited the subgroup: ' + sub.new_claims.join(', ') }, sub.new_claims.length + ' new')
          : null,
        sub.folded ? h('span', { class: 'note' }, '(folded) ' + sub.claims.length + ' claim' + (sub.claims.length === 1 ? '' : 's')) : null,
        h('span', { class: 'right' }, right),
      ),
    );
    if (sub.locations.length) details.appendChild(h('div', { class: 'locations' }, sub.locations.join(' · ')));
    if (sub.why) details.appendChild(h('div', { class: 'fold-reason' }, sub.why));
    if (sub.label_mismatch) details.appendChild(h('div', { class: 'mismatch' }, '⚠ ', sub.label_mismatch));
    details.appendChild(h('div', { class: 'fold-reason' }, (sub.folded ? 'Folded: ' : 'Open: ') + sub.fold_reason));
    details.appendChild(h('table', { class: 'claims' }, h('tbody', null, sub.claims.map(renderClaim))));
    return details;
  }

  function renderClaim(claim) {
    const result = claim.result || {};
    const details = [];
    if (result.proof) details.push(h('div', { class: 'detail' }, 'proof: ', result.proof));
    if (result.reason) details.push(h('div', { class: 'detail' }, claim.status, ': ', result.reason));
    if (result.effect) details.push(h('div', { class: 'detail' }, 'effect hint: ', result.effect, result.witness ? ' (for example ' + result.witness + ')' : ''));
    if (result.hint) details.push(h('div', { class: 'detail hint' }, 'hint: ', result.hint));
    if (claim.review === 'changed') details.push(h('div', { class: 'detail hint' }, 'The claim changed after the review.'));
    let ba = null;
    if (claim.property) {
      // A property claim states a checked fact; it changes no code.
      if (result.after) details.push(h('div', { class: 'detail' }, 'checked: ', result.after));
    } else if (result.before || result.after) {
      ba = h('div', { class: 'ba' },
        result.before ? h('del', { class: 'd' }, result.before) : null,
        result.before && result.after ? ' → ' : null,
        result.after ? h('ins', { class: 'd' }, result.after) : null);
    }
    const anchor = claim.at
      ? h('span', { class: 'anchor', title: claim.at, onclick: () => openPanel(claim.at) }, shortId(claim.at))
      : h('span', { class: 'anchor' }, claim.args && claim.args.file ? claim.args.file : '');
    return h('tr', null,
      h('td', { class: 'icon ' + claim.status, title: claim.status }, STATUS_ICONS[claim.status] || '·'),
      h('td', null,
        h('span', { class: 'op' }, claim.op), ' ', anchor, ' ', classChip(claim.class, claim.mark && claim.result ? claim.result.build_code : null),
        // The mark of a claim with no class label (I6).
        !claim.class && claim.mark ? h('span', { class: 'chip' }, claim.mark.trim()) : null,
        claim.new_in_subgroup ? [' ', h('span', { class: 'chip new', title: 'The engine added this claim after the agent edited the subgroup.' }, 'new')] : null,
        h('div', { class: 'text' }, claim.summary),
        ba, details),
      h('td', { class: 'actions' },
        claim.flow ? h('button', { type: 'button', onclick: () => navigateFlow(claim.at || claim.flow) }, 'flow view') : null,
        ' ', h('span', { class: 'mono note' }, claim.id), ' ',
        reviewBox(claim.id, claim.review === 'reviewed', '')),
    );
  }

  // ---------------------------------------------------------------- change tree

  async function loadTree() {
    state.tree = await api('/api/tree');
    renderTree();
  }

  function treeHasChange(node) {
    return node.status !== 'same' || (node.children || []).some(treeHasChange);
  }

  // `mode` is undefined (changes open, unchanged folded), 'all' (all open), or 'changes' (only changes).
  function renderTreeNode(node, mode) {
    const status = node.status || 'same';
    const row = h('span', { class: 'row st-' + status + '-row' },
      marker(status),
      h('span', { class: 'label', title: node.id, onclick: (event) => { event.preventDefault(); openPanel(node.id); } }, diffText(node.label)),
      node.diff ? h('span', { class: 'diff' }, diffText(node.diff)) : null,
      classChip(node.class, node.build_code),
      node.summary ? h('span', { class: 'summary' }, node.summary) : null,
      (node.claims || []).length ? h('span', { class: 'claim-ref' }, node.claims.join(' ')) : null,
      (node.steps || []).length ? h('span', { class: 'summary' }, (node.steps.length === 1 ? 'step ' : 'steps ') + node.steps.join(', ')) : null,
    );
    const children = (node.children || []).filter((child) => mode !== 'changes' || treeHasChange(child));
    if (!children.length) return h('li', null, h('span', { class: 'leaf-pad' }), row);
    const open = status !== 'same' || mode === 'all';
    return h('li', null,
      h('details', { open: open || undefined },
        h('summary', null, h('span', { class: 'twisty' }), row),
        h('ul', null, children.map((child) => renderTreeNode(child, mode)))));
  }

  function renderTree(mode) {
    const root = clear($('#tree'));
    if (!state.tree) return;
    root.appendChild(h('ul', { class: 'tree' }, renderTreeNode(state.tree, mode)));
  }

  // ---------------------------------------------------------------- flow view

  let mermaidReady = false;
  let renderCounter = 0;

  function setupMermaid() {
    if (typeof mermaid === 'undefined') return false;
    mermaid.initialize({
      startOnLoad: false,
      securityLevel: 'strict',
      theme: isDark() ? 'dark' : 'default',
      flowchart: { curve: 'basis', useMaxWidth: true },
    });
    mermaidReady = true;
    return true;
  }

  function navigateFlow(id) {
    const target = '#/flow/' + enc(id);
    if (location.hash === target) loadFlow(id).catch(fail);
    else location.hash = target;
  }

  // Keeps the flow history in step with the URL, so the browser buttons and the ◀ ▶ buttons agree.
  function syncFlowHistory(id) {
    const stack = state.flowStack;
    if (stack[state.flowIndex] === id) return;
    if (stack[state.flowIndex - 1] === id) state.flowIndex -= 1;
    else if (stack[state.flowIndex + 1] === id) state.flowIndex += 1;
    else {
      stack.splice(state.flowIndex + 1);
      stack.push(id);
      state.flowIndex = stack.length - 1;
    }
    updateFlowButtons();
  }

  function updateFlowButtons() {
    $('#flow-back').disabled = state.flowIndex <= 0;
    $('#flow-forward').disabled = state.flowIndex >= state.flowStack.length - 1;
  }

  function flowStep(delta) {
    const index = state.flowIndex + delta;
    if (index < 0 || index >= state.flowStack.length) return;
    state.flowIndex = index;
    updateFlowButtons();
    location.hash = '#/flow/' + enc(state.flowStack[index]);
  }

  async function loadFlow(id) {
    const data = await api('/api/flow/' + enc(id));
    data.requested = id;
    state.flowCurrent = data;
    await renderFlow(data);
  }

  // Mermaid uses light fills for the classes. A dark text color keeps the labels readable.
  function themedMermaid(text) {
    return text.replace(/^(\s*classDef \w+ [^\n]*)$/gm, '$1,color:#1a1a1a');
  }

  async function renderFlow(data) {
    const crumbs = clear($('#flow-crumbs'));
    data.breadcrumb.forEach((crumb, index) => {
      if (index > 0) crumbs.appendChild(h('span', { class: 'sep' }, '›'));
      const last = index === data.breadcrumb.length - 1;
      if (last || !crumb.id) crumbs.appendChild(h('span', { class: last ? 'current' : '' }, crumb.label));
      else crumbs.appendChild(h('a', { onclick: () => openMapAt(crumb.id), title: 'Show ' + crumb.id + ' in the workspace map' }, crumb.label));
    });
    $('#flow-note').textContent = data.synthetic
      ? 'This function has no flow view in the data. The view shows its callers and callees.'
      : 'Click a node or a stub to go there.';
    $('#flow-mermaid').textContent = show(data.mermaid);
    const box = clear($('#flow-graph'));
    if (!mermaidReady && !setupMermaid()) {
      box.appendChild(h('p', { class: 'empty' }, 'Mermaid did not load. The Mermaid text is below.'));
      return;
    }
    mermaid.initialize({ startOnLoad: false, securityLevel: 'strict', theme: isDark() ? 'dark' : 'default', flowchart: { curve: 'basis', useMaxWidth: true } });
    renderCounter += 1;
    let svg;
    try {
      ({ svg } = await mermaid.render('flow-svg-' + renderCounter, themedMermaid(show(data.mermaid))));
    } catch (error) {
      box.appendChild(h('p', { class: 'empty' }, 'Mermaid cannot draw this graph: ' + (error.message || error)));
      return;
    }
    if (state.flowCurrent !== data) return;
    // Mermaid sanitizes the SVG with securityLevel "strict".
    box.innerHTML = svg;
    for (const node of box.querySelectorAll('g.node')) {
      const id = node.dataset.id || (node.id.match(/flowchart-(.+)-\d+$/) || [])[1];
      const link = id && data.links[id];
      if (!link) continue;
      node.classList.add('clickable');
      const title = document.createElementNS('http://www.w3.org/2000/svg', 'title');
      title.textContent = show((link.flow ? 'Open the flow view of ' : 'Show ') + link.target);
      node.appendChild(title);
      node.addEventListener('click', () => {
        if (link.flow) navigateFlow(link.target);
        else openPanel(link.target);
      });
    }
  }

  // ---------------------------------------------------------------- workspace map

  async function loadGraph() {
    if (!state.graph) state.graph = await api('/api/graph');
    return state.graph;
  }

  function mapOptions() {
    return {
      level: Number($('#map-level').value),
      version: $('#map-version').value,
      tests: $('#map-tests').checked,
      impact: $('#map-impact').checked,
    };
  }

  function heatClass(changes) {
    if (!changes) return 'h0';
    if (changes === 1) return 'h1';
    if (changes <= 3) return 'h2';
    if (changes <= 8) return 'h3';
    return 'h4';
  }

  // Builds the Cytoscape elements for one level (design section 10.1).
  function mapElements(graph, options) {
    const byId = new Map(graph.nodes.map((node) => [node.id, node]));
    const inTests = (node) => {
      for (let current = node, depth = 0; current && depth < 64; current = byId.get(current.parent), depth++) {
        if (current.test) return true;
      }
      return false;
    };
    const versionOk = (status) => !(options.version === 'before' && status === 'added') && !(options.version === 'after' && status === 'removed');
    const levelOk = (node) => {
      if (options.level === 0) return node.kind === 'crate';
      if (options.level === 1) return node.kind === 'crate' || node.kind === 'module';
      return true;
    };
    const visible = new Set();
    for (const node of graph.nodes) {
      if (!levelOk(node) || !versionOk(node.status)) continue;
      if (!options.tests && inTests(node)) continue;
      visible.add(node.id);
    }
    // The nearest visible node for a node ID: the node or an ancestor.
    const represent = (id) => {
      for (let current = byId.get(id), depth = 0; current && depth < 64; current = byId.get(current.parent), depth++) {
        if (visible.has(current.id)) return current.id;
      }
      return null;
    };
    const elements = [];
    for (const node of graph.nodes) {
      if (!visible.has(node.id)) continue;
      const parent = node.parent && visible.has(node.parent) ? node.parent : undefined;
      elements.push({
        group: 'nodes',
        data: {
          id: node.id,
          parent,
          label: show(node.kind === 'external' ? node.label + (node.badges.length ? ' [' + node.badges.join(' ') + ']' : '') : node.label),
          kind: node.kind,
          status: node.status,
          changes: node.changes || 0,
          heat: heatClass(node.changes || 0),
          test: node.test,
        },
      });
    }
    const seen = new Set();
    for (const edge of graph.edges) {
      if (!versionOk(edge.status)) continue;
      let from = edge.from;
      let to = edge.to;
      if (options.level < 2 || edge.kind === 'depends') {
        from = represent(edge.from);
        to = represent(edge.to);
      }
      if (!from || !to || from === to || !visible.has(from) || !visible.has(to)) continue;
      const key = from + '\u0000' + to + '\u0000' + edge.kind;
      if (seen.has(key)) continue;
      seen.add(key);
      elements.push({ group: 'edges', data: { id: 'e' + seen.size, source: from, target: to, kind: edge.kind, status: edge.status } });
    }
    return { elements, represent };
  }

  function mapStyle() {
    const color = (name) => cssVar(name) || '#888';
    const statusColor = {
      added: color('--added'), removed: color('--removed'), changed: color('--changed'), moved: color('--moved'),
      extracted: color('--extracted'), uncovered: color('--uncovered'), same: color('--border'),
    };
    return [
      {
        selector: 'node',
        style: {
          label: 'data(label)',
          'font-family': 'ui-monospace, Menlo, Consolas, monospace',
          'font-size': 11,
          color: color('--fg'),
          'text-valign': 'center',
          'text-halign': 'center',
          shape: 'round-rectangle',
          width: (ele) => Math.max(44, String(ele.data('label')).length * 6.8 + 18),
          height: 28,
          'background-color': (ele) => color('--heat' + ele.data('heat').slice(1)),
          'border-width': (ele) => (ele.data('status') === 'same' ? 1 : 2.5),
          'border-color': (ele) => statusColor[ele.data('status')] || statusColor.same,
          'border-style': (ele) => (ele.data('status') === 'removed' ? 'dashed' : 'solid'),
        },
      },
      {
        selector: ':parent',
        style: {
          'text-valign': 'top',
          'text-halign': 'center',
          'font-weight': 'bold',
          'background-opacity': 0.35,
          padding: 14,
        },
      },
      { selector: 'node[kind = "external"]', style: { shape: 'cut-rectangle', 'background-color': color('--panel-2'), color: color('--muted') } },
      { selector: 'node[kind = "type"], node[kind = "trait"], node[kind = "other"]', style: { shape: 'rectangle' } },
      { selector: 'node[?test]', style: { 'border-style': 'dotted' } },
      {
        selector: 'edge',
        style: {
          width: 1.4,
          'curve-style': 'bezier',
          'target-arrow-shape': 'triangle',
          'arrow-scale': 0.9,
          'line-color': (ele) => (ele.data('status') === 'same' ? color('--muted') : statusColor[ele.data('status')]),
          'target-arrow-color': (ele) => (ele.data('status') === 'same' ? color('--muted') : statusColor[ele.data('status')]),
          'line-style': (ele) => (ele.data('status') === 'removed' ? 'dashed' : ele.data('kind') === 'depends' ? 'dotted' : 'solid'),
          opacity: 0.85,
        },
      },
      { selector: '.dim', style: { opacity: 0.25 } },
      { selector: 'node.impact', style: { 'border-color': color('--impact'), 'border-width': 3.5, opacity: 1 } },
      { selector: 'edge.impact', style: { 'line-color': color('--impact'), 'target-arrow-color': color('--impact'), width: 2.6, opacity: 1 } },
      { selector: 'node.target', style: { 'border-color': color('--impact'), 'border-width': 5, 'border-style': 'double', opacity: 1 } },
      { selector: 'node:selected', style: { 'overlay-color': color('--accent'), 'overlay-opacity': 0.18 } },
    ];
  }

  function styleMap() {
    if (state.cy) state.cy.style(mapStyle());
  }

  function layoutMap() {
    const cy = state.cy;
    const elk = {
      name: 'elk',
      fit: true,
      padding: 24,
      nodeDimensionsIncludeLabels: true,
      elk: {
        algorithm: 'layered',
        'elk.direction': 'DOWN',
        'elk.hierarchyHandling': 'INCLUDE_CHILDREN',
        'elk.layered.spacing.nodeNodeBetweenLayers': 46,
        'elk.spacing.nodeNode': 28,
      },
    };
    try {
      const layout = cy.layout(elk);
      layout.one('layoutstop', () => focusMap());
      layout.run();
    } catch (error) {
      console.warn('The elk layout failed. The map uses the breadthfirst layout.', error);
      cy.layout({ name: 'breadthfirst', directed: true, padding: 24 }).run();
      focusMap();
    }
  }

  function focusMap() {
    const cy = state.cy;
    if (!cy) return;
    if (state.mapFocus) {
      const node = cy.getElementById(state.mapFocus);
      state.mapFocus = null;
      if (node.nonempty()) {
        cy.fit(node.union(node.descendants()), 40);
        node.select();
        return;
      }
    }
    cy.fit(undefined, 24);
  }

  function nodeById(id) {
    return state.graph && state.graph.nodes.find((node) => node.id === id);
  }

  // All the functions that can reach `id` through call edges.
  function transitiveCallers(id, options) {
    const callers = new Map();
    for (const edge of state.graph.edges) {
      if (edge.kind !== 'call') continue;
      if (options.version === 'before' && edge.status === 'added') continue;
      if (options.version === 'after' && edge.status === 'removed') continue;
      if (!callers.has(edge.to)) callers.set(edge.to, []);
      callers.get(edge.to).push(edge.from);
    }
    const seen = new Set();
    const queue = [id];
    while (queue.length) {
      const current = queue.shift();
      for (const caller of callers.get(current) || []) {
        if (caller !== id && !seen.has(caller)) {
          seen.add(caller);
          queue.push(caller);
        }
      }
    }
    return seen;
  }

  function applyImpact() {
    const cy = state.cy;
    if (!cy) return;
    cy.elements().removeClass('impact target dim');
    const options = mapOptions();
    const note = $('#map-note');
    if (!options.impact) {
      note.textContent = 'Click a function to open its flow view. Click a crate or a module to go down one level.';
      return;
    }
    if (!state.mapSelected) {
      note.textContent = 'Impact: click a changed node to mark all the callers that can reach it.';
      return;
    }
    const { represent } = state.mapRepresent;
    const target = state.mapSelected;
    const callers = transitiveCallers(target, options);
    const marked = new Set();
    for (const caller of callers) {
      const shown = represent(caller);
      if (shown) marked.add(shown);
    }
    const targetShown = represent(target);
    cy.elements().addClass('dim');
    for (const id of marked) cy.getElementById(id).removeClass('dim').addClass('impact');
    if (targetShown) cy.getElementById(targetShown).removeClass('dim impact').addClass('target');
    cy.edges().forEach((edge) => {
      const source = edge.source().id();
      const sink = edge.target().id();
      if (marked.has(source) && (marked.has(sink) || sink === targetShown)) edge.removeClass('dim').addClass('impact');
    });
    // Keep the parents of marked nodes readable.
    cy.nodes(':parent').forEach((parent) => {
      if (parent.descendants().some((child) => !child.hasClass('dim'))) parent.removeClass('dim');
    });
    const node = nodeById(target);
    const label = node ? node.label : target;
    note.textContent =
      'Impact of ' + label + ': ' + callers.size + ' caller' + (callers.size === 1 ? '' : 's') + ' can reach it.' +
      (node && node.status === 'same' ? ' This node has no change.' : '');
  }

  function buildMap() {
    const graph = state.graph;
    const options = mapOptions();
    const { elements, represent } = mapElements(graph, options);
    state.mapRepresent = { represent };
    if (typeof cytoscape === 'undefined') {
      clear($('#map')).appendChild(h('p', { class: 'empty' }, 'Cytoscape did not load.'));
      return;
    }
    if (state.cy) state.cy.destroy();
    const cy = cytoscape({ container: $('#map'), elements, style: mapStyle(), layout: { name: 'preset' }, minZoom: 0.1, maxZoom: 4 });
    state.cy = cy;
    cy.on('tap', 'node', (event) => {
      const data = event.target.data();
      const opts = mapOptions();
      if (opts.impact) {
        state.mapSelected = data.id;
        applyImpact();
        openPanel(data.id);
        return;
      }
      if (data.kind === 'function') {
        navigateFlow(data.id);
      } else if ((data.kind === 'crate' || data.kind === 'module') && opts.level < 2) {
        $('#map-level').value = String(opts.level + 1);
        state.mapFocus = data.id;
        buildMap();
      } else {
        openPanel(data.id);
      }
    });
    cy.on('tap', (event) => {
      if (event.target === cy && mapOptions().impact) {
        state.mapSelected = null;
        applyImpact();
      }
    });
    layoutMap();
    applyImpact();
  }

  async function openMap() {
    await loadGraph();
    if (!state.cy) buildMap();
    else {
      state.cy.resize();
      focusMap();
    }
  }

  // Opens the map at the level below a crate or a module, with the node in focus.
  function openMapAt(id) {
    const node = nodeById(id);
    const level = node && node.kind === 'crate' ? 1 : 2;
    $('#map-level').value = String(level);
    state.mapFocus = id;
    if (state.cy) {
      state.cy.destroy();
      state.cy = null;
    }
    location.hash = '#/map';
    if (state.view === 'map') openMap().catch(fail);
  }

  // ---------------------------------------------------------------- details panel

  // A line diff with the longest common subsequence. The texts are small.
  function lineDiff(before, after) {
    const a = before.split('\n');
    const b = after.split('\n');
    const n = a.length;
    const m = b.length;
    if (n * m > 400000) return null;
    const table = Array.from({ length: n + 1 }, () => new Uint32Array(m + 1));
    for (let i = n - 1; i >= 0; i--) {
      for (let j = m - 1; j >= 0; j--) {
        table[i][j] = a[i] === b[j] ? table[i + 1][j + 1] + 1 : Math.max(table[i + 1][j], table[i][j + 1]);
      }
    }
    const out = [];
    let i = 0;
    let j = 0;
    while (i < n && j < m) {
      if (a[i] === b[j]) {
        out.push([' ', a[i]]);
        i++;
        j++;
      } else if (table[i + 1][j] >= table[i][j + 1]) {
        out.push(['-', a[i++]]);
      } else {
        out.push(['+', b[j++]]);
      }
    }
    while (i < n) out.push(['-', a[i++]]);
    while (j < m) out.push(['+', b[j++]]);
    return out;
  }

  function sourceBlock(before, after) {
    if (before === null && after === null) return h('p', { class: 'source-note' }, 'No source text.');
    let lines;
    let note;
    if (before === null) {
      lines = after.split('\n').map((line) => ['+', line]);
      note = 'The node is new: it is not in the before version.';
    } else if (after === null) {
      lines = before.split('\n').map((line) => ['-', line]);
      note = 'The node is removed: it is not in the after version.';
    } else if (before === after) {
      lines = after.split('\n').map((line) => [' ', line]);
      note = 'The source text did not change.';
    } else {
      lines = lineDiff(before, after) || after.split('\n').map((line) => [' ', line]);
      note = 'Before → after.';
    }
    return [
      h('p', { class: 'source-note' }, note),
      h('pre', { class: 'source' },
        lines.map(([sign, text]) => h('div', { class: 'line' + (sign === '+' ? ' add' : sign === '-' ? ' del' : '') }, h('span', { class: 'sign' }, sign), text))),
    ];
  }

  function nodeLinks(list) {
    if (!list || !list.length) return h('span', { class: 'note' }, 'none');
    return h('span', { class: 'links' },
      list.map((node) => h('a', { title: node.id, onclick: () => openPanel(node.id) }, marker(node.status), ' ', node.label || shortId(node.id))));
  }

  async function openPanel(id, keepScroll) {
    const panel = $('#panel');
    const body = $('#panel-body');
    const scroll = panel.scrollTop;
    panel.hidden = false;
    panel.dataset.node = id;
    let info;
    try {
      info = await api('/api/node/' + enc(id));
    } catch (error) {
      clear(body).appendChild(h('p', { class: 'empty' }, 'No data for ' + id + ': ' + error.message));
      return;
    }
    if (panel.dataset.node !== id) return;
    const [before, after, neighbors] = await Promise.all([
      api('/api/source/' + enc(id) + '?v=before').catch(() => null),
      api('/api/source/' + enc(id) + '?v=after').catch(() => null),
      info.graph ? api('/api/node/' + enc(id) + '/neighbors').catch(() => null) : Promise.resolve(null),
    ]);
    if (panel.dataset.node !== id) return;
    clear(body);
    body.appendChild(h('h2', null, marker(info.status), ' ', info.label));
    body.appendChild(h('div', { class: 'mono note' }, info.id));
    body.appendChild(
      h('div', { class: 'note' }, info.kind, info.status ? ' · ' + info.status : '', ' · module ', info.module, info.file ? ' · file ' + info.file : '', ' ', classChip(info.class, info.build_code)),
    );
    const actions = [];
    if (info.flow) actions.push(h('button', { type: 'button', onclick: () => navigateFlow(id) }, 'Open flow view'));
    if (info.graph) actions.push(h('button', { type: 'button', onclick: () => openMapAt(info.graph.parent || id) }, 'Show in map'));
    if (info.parent) actions.push(h('button', { type: 'button', onclick: () => openPanel(info.parent) }, 'Parent'));
    if (actions.length) body.appendChild(h('div', { class: 'actions' }, actions));
    if (info.tree && (info.tree.diff || info.tree.summary)) {
      body.appendChild(h('div', null, info.tree.diff ? h('span', { class: 'diff mono' }, diffText(info.tree.diff)) : null, ' ', info.tree.summary ? h('span', { class: 'note' }, info.tree.summary) : null));
    }
    if (neighbors) {
      body.appendChild(h('h3', null, 'Callers'));
      body.appendChild(nodeLinks(neighbors.callers));
      body.appendChild(h('h3', null, 'Callees'));
      body.appendChild(nodeLinks(neighbors.callees));
      if (neighbors.impact.length) {
        body.appendChild(h('h3', null, 'Impact'));
        body.appendChild(h('div', { class: 'note' }, neighbors.impact.length + ' function(s) can reach this node: ', neighbors.impact.map(shortId).join(', ')));
      }
    }
    if (info.claims.length) {
      body.appendChild(h('h3', null, 'Claims'));
      body.appendChild(h('table', { class: 'claims' }, h('tbody', null, info.claims.map(renderClaim))));
    }
    body.appendChild(h('h3', null, 'Source'));
    const beforeText = before ? before.text : null;
    const afterText = after ? after.text : null;
    if (before && before.from !== 'sources' && before.key) {
      body.appendChild(h('p', { class: 'source-note' }, 'From ', before.from === 'claim' ? 'claim ' + before.claim : before.key, '.'));
    }
    append(body, [sourceBlock(beforeText, afterText)]);
    if (keepScroll) panel.scrollTop = scroll;
    else panel.scrollTop = 0;
  }

  function closePanel() {
    $('#panel').hidden = true;
    delete $('#panel').dataset.node;
    if (location.hash.startsWith('#/node/')) history.replaceState(null, '', '#/' + state.view);
    if (state.cy) setTimeout(() => state.cy.resize(), 0);
  }

  // ---------------------------------------------------------------- search

  let searchTimer = null;
  let searchSeq = 0;

  function pickHit(hit) {
    hideSearch();
    if (hit.flow && (hit.kind === 'function' || hit.kind === 'statement')) navigateFlow(hit.id);
    else openPanel(hit.id);
  }

  function hideSearch() {
    $('#search-results').hidden = true;
  }

  async function runSearch() {
    const text = $('#search').value.trim();
    const box = $('#search-results');
    if (!text) {
      hideSearch();
      return;
    }
    const seq = ++searchSeq;
    let data;
    try {
      data = await api('/api/search?q=' + enc(text));
    } catch (error) {
      fail(error);
      return;
    }
    if (seq !== searchSeq) return;
    clear(box);
    for (const error of data.query.errors) box.appendChild(h('div', { class: 'error' }, '⚠ ', error));
    if (data.paths.length) {
      box.appendChild(h('div', { class: 'heading' }, data.paths.length + ' call path' + (data.paths.length === 1 ? '' : 's')));
      for (const path of data.paths) {
        const parts = [];
        path.forEach((id, index) => {
          if (index) parts.push(' → ');
          parts.push(h('a', { title: id, onclick: () => { hideSearch(); navigateFlow(id); } }, shortId(id)));
        });
        box.appendChild(h('div', { class: 'path' }, parts));
      }
    }
    if (data.results.length) {
      box.appendChild(h('div', { class: 'heading' }, data.total + ' result' + (data.total === 1 ? '' : 's') + (data.truncated ? ' (first ' + data.results.length + ')' : '')));
    }
    data.results.forEach((hit, index) => {
      box.appendChild(
        h('div', { class: 'hit' + (index === 0 ? ' focus' : ''), onclick: () => pickHit(hit) },
          marker(hit.status || 'same'),
          h('span', null, h('b', null, hit.label), ' ', classChip(hit.class, hit.build_code)),
          h('span', { class: 'meta' }, hit.kind, hit.claims.length ? ' · ' + hit.claims.join(' ') : '', hit.matched.length ? ' · ' + hit.matched.join(', ') : ''),
          h('span', { class: 'id' }, hit.id)),
      );
    });
    if (!data.results.length && !data.paths.length && !data.query.errors.length) box.appendChild(h('div', { class: 'heading' }, 'No results.'));
    box.hidden = false;
    box.firstResult = data.results[0] || null;
  }

  // ---------------------------------------------------------------- start

  function bind() {
    $('#theme').addEventListener('click', () => {
      const next = THEMES[(THEMES.indexOf(storedTheme()) + 1) % THEMES.length];
      applyTheme(next);
      onThemeChange();
    });
    if (window.matchMedia) {
      const query = window.matchMedia('(prefers-color-scheme: dark)');
      if (query.addEventListener) query.addEventListener('change', onThemeChange);
    }
    window.addEventListener('hashchange', route);
    $('#flow-back').addEventListener('click', () => flowStep(-1));
    $('#flow-forward').addEventListener('click', () => flowStep(1));
    $('#flow-copy').addEventListener('click', () => {
      const text = $('#flow-mermaid').textContent;
      if (navigator.clipboard) navigator.clipboard.writeText(text).then(() => toast('The Mermaid text is on the clipboard.'), fail);
    });
    $('#tree-expand').addEventListener('click', () => renderTree('all'));
    $('#tree-changed').addEventListener('click', () => renderTree('changes'));
    $('#panel-close').addEventListener('click', closePanel);
    for (const id of ['#map-level', '#map-version', '#map-tests']) {
      $(id).addEventListener('change', () => { if (state.graph) buildMap(); });
    }
    $('#map-impact').addEventListener('change', () => {
      if (!$('#map-impact').checked) state.mapSelected = null;
      applyImpact();
    });
    $('#map-fit').addEventListener('click', () => { if (state.cy) state.cy.fit(undefined, 24); });
    const input = $('#search');
    input.addEventListener('input', () => {
      clearTimeout(searchTimer);
      searchTimer = setTimeout(runSearch, 180);
    });
    input.addEventListener('focus', () => { if (input.value.trim()) runSearch(); });
    input.addEventListener('keydown', (event) => {
      if (event.key === 'Escape') {
        hideSearch();
        input.blur();
      } else if (event.key === 'Enter') {
        clearTimeout(searchTimer);
        const box = $('#search-results');
        if (!box.hidden && box.firstResult) pickHit(box.firstResult);
        else runSearch();
      }
    });
    document.addEventListener('click', (event) => {
      if (!event.target.closest('.search')) hideSearch();
    });
    document.addEventListener('keydown', (event) => {
      const typing = /^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement && document.activeElement.tagName);
      if (event.key === '/' && !typing) {
        event.preventDefault();
        input.focus();
        input.select();
      } else if (event.key === 'Escape' && !typing) {
        closePanel();
      }
    });
  }

  async function start() {
    applyTheme(storedTheme());
    bind();
    updateFlowButtons();
    setupMermaid();
    try {
      await loadChangeset();
    } catch (error) {
      fail(error);
    }
    route();
  }

  // For the browser console: the state of the UI.
  window.semilla = { state, navigateFlow, openPanel, buildMap };

  start();
})();
