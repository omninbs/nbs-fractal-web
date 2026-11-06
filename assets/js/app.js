// 步骤目录：键与后端 steps 字段的取值一致；fields 声明该步用到哪些参数字段
// （`split` 是伪字段，实际提交时展开成 breaks / halves 或 no_breaks）
const CATALOG = {
  compact: {
    name: 'Layout (Compact)',
    desc: 'Compact variable-length packed structure',
    fields: ['coarse', 'wrap', 'gap', 'split', 'floor'],
  },
  linear: {
    name: 'Layout (Linear)',
    desc: 'Foldable timeline with side branches',
    fields: ['wrap', 'gap', 'split', 'floor'],
    gravity: true,
  },
  fishbone: {
    name: 'Layout (Fishbone)',
    desc: 'Linear timeline with side branches',
    fields: ['gap', 'split', 'floor'],
    gravity: true,
  },
  torrent: {
    name: 'Layout (Torrent)',
    desc: 'Tunnel of slices with pan and volume',
    fields: ['ticks', 'tracks', 'all_tracks'],
    experimental: true,
  },
  decompose: {
    name: 'Decompose (Auto)',
    desc: 'Automatically decompose NBS files into TEC groups',
    fields: ['layers'],
  },
  match: {
    name: 'Decompose (Manual)',
    desc: 'Manually decompose NBS files into TEC groups',
    fields: ['rules'],
  },
  tapped: {
    name: 'Layout (Tapped)',
    desc: 'Tapped delay line simulating TEC',
    fields: ['wrap', 'gap', 'floor', 'no_balance'],
  },
};

// 添加步骤页的分组顺序（同一组内按 CATALOG 声明顺序）：先常规布局，
// 后接 TEC 相关的两步
const GROUPS = [
  { label: 'Layout', types: ['compact', 'linear', 'fishbone', 'torrent'] },
  { label: 'Decompose', types: ['decompose', 'match'] },
  { label: 'Tapped', types: ['tapped'] },
];

function app() {
  return {
    catalog: CATALOG,
    // 管线：从上到下就是执行顺序
    steps: [],
    nextId: 1,
    // 当前标签页：`'add'` / `'song'` / 步骤 id
    active: 'add',
    // state
    ready: false,
    file: null,
    fileName: '',
    dragOver: false,
    loading: false,
    error: '',
    downloadUrl: '',
    downloadName: '',

    init() {
      // 从 `#<step>.<step>` 恢复管线，便于分享链接
      const hash = location.hash.slice(1);
      if (hash) this.restore(hash);
      // 浏览器前进/后退切换管线
      addEventListener('hashchange', () => {
        const next = location.hash.slice(1);
        if (next !== this.hashOf()) this.restore(next);
      });
      // 等首次 x-show 求值后再置就绪（早于此刻会跳过过渡），主界面才淡入
      this.$nextTick(() => { this.ready = true; });
    },

    // 目录与步骤查询
    typeDef(type) {
      return this.catalog[type];
    },
    // 当前步骤是否用到某参数（模板据此显隐）
    has(step, field) {
      return this.typeDef(step.type).fields.includes(field);
    },
    // 某步骤类型能否接在给定管线之后：上游只有一条链，形状极窄
    canFollow(steps, type) {
      const last = steps.length ? steps[steps.length - 1].type : '';
      if (!last) return type in this.catalog;
      if (last === 'decompose' || last === 'match') return type === 'tapped';
      return false;
    },
    get addable() {
      return Object.keys(this.catalog).filter((type) => this.canFollow(this.steps, type));
    },
    get canAdd() {
      return this.addable.length > 0;
    },
    get addGroups() {
      const addable = this.addable;
      return GROUPS.map((group) => ({
        label: group.label,
        types: group.types.filter((type) => addable.includes(type)),
      })).filter((group) => group.types.length > 0);
    },

    // 新步骤的默认参数：wrap 跟着布局类型定，别的用同一套默认值
    defaults(type) {
      return {
        coarse: 0,
        wrap: type === 'linear' ? 0 : 16,
        gap: 0,
        floor: 'none',
        layers: 3,
        rules: '',
        ticks: '',
        tracks: '',
        all_tracks: false,
        halves: false,
        breaks: false,
        no_breaks: false,
        no_balance: false,
      };
    },

    addStep(type) {
      const step = { id: this.nextId++, type, params: this.defaults(type) };
      this.steps.push(step);
      this.active = step.id;
      this.resetResult();
      this.syncHash();
    },
    // 删掉某步时连同其后继一起删，不留悬空步骤
    removeStep(step) {
      const index = this.steps.findIndex((s) => s.id === step.id);
      if (index < 0) return;
      this.steps.splice(index);
      this.active = this.steps.length ? this.steps[this.steps.length - 1].id : 'add';
      this.resetResult();
      this.syncHash();
    },

    // 展示用派生值，避免模板里写长表达式
    get fileSize() {
      return this.file ? (this.file.size / 1024).toFixed(1) + ' KB' : '';
    },
    gapHelp() {
      return 'Block spacing between adjacent tracks (0 = interlocked)';
    },
    wrapPlaceholder(step) {
      return step.type === 'linear' ? '0' : '16';
    },
    splitHelp(step) {
      if (step.type === 'compact') {
        return 'Breaks splits on blank layers; Halves splits even/odd game ticks into separate tracks';
      }
      return step.params.no_breaks
        ? 'Keep the song on one track, ignoring blank layers'
        : 'Split the song into tracks on blank layers';
    },
    floorHelp(step) {
      return this.typeDef(step.type).gravity
        ? 'Floor platform mode'
        : 'Add a full floor platform below the build';
    },

    // hash 与管线同步（`#decompose.tapped`）
    hashOf() {
      return this.steps.map((step) => step.type).join('.');
    },
    syncHash() {
      const next = this.hashOf();
      if (location.hash.slice(1) === next) return;
      if (next) {
        location.hash = next;
      } else {
        history.replaceState(null, '', location.pathname + location.search);
      }
    },
    // 从 hash 重建管线；非法前缀之后的部分直接丢弃
    restore(text) {
      const steps = [];
      for (const type of text.split('.').filter(Boolean)) {
        if (!this.canFollow(steps, type)) break;
        steps.push({ id: this.nextId++, type, params: this.defaults(type) });
      }
      this.steps = steps;
      this.active = steps.length ? steps[0].id : 'add';
      this.resetResult();
    },

    resetResult() {
      if (this.downloadUrl) URL.revokeObjectURL(this.downloadUrl);
      this.downloadUrl = '';
      this.downloadName = '';
      this.error = '';
    },

    // file select handler
    handleFile(e) {
      const f = e.target.files?.[0];
      if (f) this.setFile(f);
    },
    handleDrop(e) {
      this.dragOver = false;
      const f = e.dataTransfer?.files?.[0];
      if (f) this.setFile(f);
    },
    setFile(f) {
      // `.tec.nbs` 也是 `.nbs`：分组过的文件可以直接作为 tapped 的输入
      if (!f.name.endsWith('.nbs')) {
        this.error = 'Please select a .nbs file';
        return;
      }
      this.file = f;
      this.fileName = f.name;
      this.error = '';
      this.downloadUrl = '';
    },

    // 提交字段由各步骤声明驱动；参数名在允许的管线里互不重叠，可平铺
    collect() {
      const fd = new FormData();
      fd.append('steps', this.steps.map((step) => step.type).join(','));
      for (const step of this.steps) {
        const params = step.params;
        for (const field of this.typeDef(step.type).fields) {
          if (field === 'split') {
            if (step.type === 'compact') {
              fd.append('breaks', params.breaks);
              fd.append('halves', params.halves);
            } else {
              fd.append('no_breaks', params.no_breaks);
            }
            continue;
          }
          fd.append(field, params[field]);
        }
      }
      return fd;
    },

    // generate
    async generate() {
      if (!this.file || this.loading || !this.steps.length) return;
      this.loading = true;
      this.error = '';
      this.downloadUrl = '';
      this.downloadName = '';

      const fd = this.collect();
      fd.append('file', this.file);

      try {
        const res = await fetch('./api/generate', { method: 'POST', body: fd });
        if (!res.ok) {
          const text = await res.text();
          throw new Error(text || 'Conversion failed');
        }
        const blob = await res.blob();
        // extract filename from Content-Disposition header
        const disp = res.headers.get('Content-Disposition');
        const match = disp?.match(/filename="?(.+?)"?$/);
        this.downloadName = match?.[1] || 'converted.litematic';
        this.downloadUrl = URL.createObjectURL(blob);
      } catch (err) {
        this.error = err.message;
      } finally {
        this.loading = false;
      }
    }
  };
}
