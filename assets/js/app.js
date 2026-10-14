function app() {
  return {
    // layout modes：value 与后端 mode 参数一致；fields 声明该模式用到的参数字段
    modes: [
      { value: 'compact', name: 'Layout (Compact)', desc: 'Compact variable-length packed structure', fields: ['coarse', 'wrap', 'gap', 'split', 'floor'] },
      { value: 'linear', name: 'Layout (Linear)', desc: 'Foldable timeline with side branches', fields: ['wrap', 'gap', 'split', 'floor'], gravity: true },
      { value: 'fishbone', name: 'Layout (Fishbone)', desc: 'Linear timeline with side branches', fields: ['gap', 'split', 'floor'], gravity: true },
      { value: 'cascade', name: 'Layout (Cascade)', desc: 'Multi-track timeline with pan and volume', experimental: true, fields: ['gap', 'floor'], gravity: true },
      { value: 'torrent', name: 'Layout (Torrent)', desc: 'Tunnel of slices with pan and volume', experimental: true, fields: ['ticks', 'tracks', 'all_tracks'] },
      { value: 'decompose', name: 'Decompose (Auto)', desc: 'Automatically decompose NBS files into TEC groups', fields: ['layers'] },
      { value: 'match', name: 'Decompose (Manual)', desc: 'Manually decompose NBS files into TEC groups', fields: ['rules'] },
      { value: 'tapped', name: 'Layout (Tapped)', desc: 'Tapped delay line simulating TEC', fields: ['wrap', 'gap', 'floor'] },
    ],
    // 提交给后端的字段，键名与状态名一致
    submit: ['mode', 'coarse', 'wrap', 'gap', 'floor', 'layers', 'rules', 'ticks', 'tracks', 'all_tracks', 'halves', 'breaks', 'no_breaks'],
    // state
    ready: false,
    file: null,
    fileName: '',
    dragOver: false,
    mode: 'compact',
    coarse: 0,
    wrap: 16,
    gap: 0,
    floor: 'none',
    layers: 2,
    rules: '',
    ticks: '',
    tracks: '',
    all_tracks: false,
    halves: false,
    breaks: false,
    no_breaks: false,
    loading: false,
    error: '',
    downloadUrl: '',
    downloadName: '',

    init() {
      // 切换布局模式时重置默认值（linear: wrap=0，其余 wrap=16）
      this.$watch('mode', (value) => {
        this.wrap = value === 'linear' ? 0 : 16;
        // gravity 档只属于部分模式，换走时退回 none
        if (!this.current.gravity && this.floor === 'gravity') {
          this.floor = 'none';
        }
        // 同步到 hash，便于分享链接（如 #compact）
        if (location.hash.slice(1) !== value) location.hash = value;
      });
      // 从 #<mode> 恢复初始模式，仅接受已定义的模式
      const hash = location.hash.slice(1);
      if (this.modes.some((m) => m.value === hash)) this.mode = hash;
      // 浏览器前进/后退切换模式
      addEventListener('hashchange', () => {
        const h = location.hash.slice(1);
        if (this.modes.some((m) => m.value === h)) this.mode = h;
      });
      // 等首次 x-show 求值后再置就绪（早于此刻会跳过过渡），主界面才淡入
      this.$nextTick(() => { this.ready = true; });
    },

    // 当前模式定义
    get current() {
      return this.modes.find((m) => m.value === this.mode);
    },
    // 当前模式是否用到某参数（模板据此显隐）
    show(field) {
      return this.current.fields.includes(field);
    },
    // 当前模式是否为实验性
    get experimental() {
      return !!this.current.experimental;
    },

    // 展示用派生值，避免模板里写长表达式
    get fileSize() {
      return this.file ? (this.file.size / 1024).toFixed(1) + ' KB' : '';
    },
    get gapHelp() {
      return 'Block spacing between adjacent tracks (0 = interlocked)';
    },
    get wrapPlaceholder() {
      return this.mode === 'linear' ? '0' : '16';
    },
    get splitHelp() {
      if (this.mode === 'compact') {
        return 'Breaks splits on blank layers; Halves splits even/odd game ticks into separate tracks';
      }
      return this.no_breaks
        ? 'Keep the song on one track, ignoring blank layers'
        : 'Split the song into tracks on blank layers';
    },
    get floorHelp() {
      return this.current.gravity
        ? 'Floor platform mode'
        : 'Add a full floor platform below the build';
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
      if (!f.name.endsWith('.nbs')) {
        this.error = 'Please select a .nbs file';
        return;
      }
      this.file = f;
      this.fileName = f.name;
      this.error = '';
      this.downloadUrl = '';
    },
    // generate
    async generate() {
      if (!this.file || this.loading) return;
      this.loading = true;
      this.error = '';
      this.downloadUrl = '';
      this.downloadName = '';

      const fd = new FormData();
      fd.append('file', this.file);
      for (const key of this.submit) fd.append(key, this[key]);

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
