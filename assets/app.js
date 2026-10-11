function app() {
  return {
    // layout modes：value 与后端 mode 参数一致
    modes: [
      { value: 'compact', name: 'Layout (Compact)', desc: 'Compact variable-length packed structure' },
      { value: 'linear', name: 'Layout (Linear)', desc: 'Foldable timeline with side branches' },
      { value: 'fishbone', name: 'Layout (Fishbone)', desc: 'Linear timeline with side branches' },
      { value: 'cascade', name: 'Layout (Cascade)', desc: 'Multi-track timeline with pan and volume' },
      { value: 'decompose', name: 'Decompose', desc: 'Automatically decompose NBS files into TEC groups' },
      { value: 'match', name: 'Match', desc: 'Manually decompose NBS files into TEC groups' },
      { value: 'tapped', name: 'Layout (Tapped)', desc: 'Simulate TEC via tapped delay line' },
    ],
    // state
    ready: false,
    file: null,
    fileName: '',
    dragOver: false,
    mode: 'compact',
    coarse: 0,
    wrap: 16,
    gap: 0,
    floorMode: 'none',
    layers: 2,
    rules: '',
    halves: false,
    breaks: false,
    noBreaks: false,
    loading: false,
    error: '',
    downloadUrl: '',
    downloadName: '',

    init() {
      // 切换布局模式时重置默认值（linear: wrap=0，其余 wrap=16）
      this.$watch('mode', () => {
        this.wrap = this.mode === 'linear' ? 0 : 16;
        // gravity 只属于 linear / fishbone / cascade，换走时退回 none
        if (!this.gravityFloor && this.floorMode === 'gravity') {
          this.floorMode = 'none';
        }
      });
      // 等首次 x-show 求值后再置就绪（早于此刻会跳过过渡），主界面才淡入
      this.$nextTick(() => { this.ready = true; });
    },

    // 只有直接产出布局的模式才显示 Wrap/Gap/Floor；decompose/match 只出中间 NBS
    get layoutMode() {
      return this.mode === 'compact' || this.mode === 'linear'
        || this.mode === 'fishbone' || this.mode === 'cascade'
        || this.mode === 'tapped';
    },
    // Splitting 适用于 compact / linear / fishbone
    get breakable() {
      return this.mode === 'compact' || this.mode === 'linear'
        || this.mode === 'fishbone';
    },
    // 有 gravity 档的 Floor 模式：linear / fishbone / cascade
    get gravityFloor() {
      return this.mode === 'linear' || this.mode === 'fishbone' || this.mode === 'cascade';
    },

    // 单个参数字段的显隐，模板只读结果
    get showWrap() {
      return this.layoutMode && this.mode !== 'fishbone' && this.mode !== 'cascade';
    },
    get showGap() {
      return this.layoutMode && this.mode !== 'cascade';
    },

    // 展示用派生值，避免模板里写表达式
    get fileSize() {
      return this.file ? (this.file.size / 1024).toFixed(1) + ' KB' : '';
    },
    get wrapPlaceholder() {
      return this.mode === 'linear' ? '0' : '16';
    },
    get gapHelp() {
      return this.mode === 'linear' || this.mode === 'fishbone'
        ? 'Block spacing between adjacent tracks (0 = interlocked)'
        : 'Block spacing between adjacent rows (0 = interlocked)';
    },
    get splitHelp() {
      if (this.mode === 'compact') {
        return 'Breaks splits on blank layers; Halves splits even/odd game ticks into separate tracks';
      }
      return this.noBreaks
        ? 'Keep the song on one track, ignoring blank layers'
        : 'Split the song into tracks on blank layers';
    },
    get floorHelp() {
      return this.gravityFloor
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
    // convert
    async convert() {
      if (!this.file || this.loading) return;
      this.loading = true;
      this.error = '';
      this.downloadUrl = '';
      this.downloadName = '';

      const fd = new FormData();
      fd.append('file', this.file);
      const params = {
        mode: this.mode, coarse: this.coarse, wrap: this.wrap,
        gap: this.gap, floor: this.floorMode, layers: this.layers,
        rules: this.rules, halves: this.halves, breaks: this.breaks,
        no_breaks: this.noBreaks,
      };
      for (const [k, v] of Object.entries(params)) fd.append(k, v);

      try {
        const res = await fetch('./api/convert', { method: 'POST', body: fd });
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
