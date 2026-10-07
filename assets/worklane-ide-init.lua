-- Worklane's shared Neovim configuration. User data remains in ~/.local/share/nvim.
vim.g.mapleader = ' '
vim.opt.mouse = 'a'
vim.opt.mousemodel = 'extend'
vim.opt.termguicolors = true
vim.opt.number = true
vim.opt.relativenumber = true
vim.opt.cursorline = true
vim.opt.signcolumn = 'yes'
vim.opt.splitright = true
vim.opt.splitbelow = true
vim.opt.equalalways = false
vim.opt.scrolloff = 5
vim.opt.sidescrolloff = 5
vim.opt.updatetime = 250
vim.opt.timeoutlen = 400
vim.opt.laststatus = 3
vim.opt.showtabline = 2
vim.opt.wrap = false
vim.opt.breakindent = true
vim.opt.undofile = true
vim.opt.confirm = true
vim.opt.fillchars = { eob = ' ', vert = '│' }

local config_dir = vim.fn.expand('~/.config/worklane/nvim')
local palette = dofile(config_dir .. '/palette.lua')
local function apply_highlights()
  vim.api.nvim_set_hl(0, 'Normal', { fg = palette.text, bg = palette.bg })
  vim.api.nvim_set_hl(0, 'NormalFloat', { fg = palette.text, bg = palette.panel })
  vim.api.nvim_set_hl(0, 'FloatBorder', { fg = palette.border, bg = palette.panel })
  vim.api.nvim_set_hl(0, 'CursorLine', { bg = palette.selection })
  vim.api.nvim_set_hl(0, 'Visual', { bg = palette.selection })
  vim.api.nvim_set_hl(0, 'LineNr', { fg = palette.muted, bg = palette.bg })
  vim.api.nvim_set_hl(0, 'CursorLineNr', { fg = palette.text, bg = palette.selection, bold = true })
  vim.api.nvim_set_hl(0, 'Comment', { fg = palette.muted, italic = true })
  vim.api.nvim_set_hl(0, 'String', { fg = palette.green })
  vim.api.nvim_set_hl(0, 'Function', { fg = palette.blue })
  vim.api.nvim_set_hl(0, 'Keyword', { fg = palette.violet })
  vim.api.nvim_set_hl(0, 'Type', { fg = palette.teal })
  vim.api.nvim_set_hl(0, 'DiagnosticError', { fg = palette.red })
  vim.api.nvim_set_hl(0, 'DiagnosticWarn', { fg = palette.amber })
end
apply_highlights()

local lazy_dir = vim.fn.stdpath('data') .. '/lazy/lazy.nvim'
if not vim.uv.fs_stat(lazy_dir) then
  local result = vim.fn.system({ 'git', 'clone', '--filter=blob:none', '--branch=v11.17.5',
    'https://github.com/folke/lazy.nvim.git', lazy_dir })
  if vim.v.shell_error ~= 0 then error('Cannot install lazy.nvim: ' .. result) end
end
vim.opt.rtp:prepend(lazy_dir)
local pins = {
  ['nvim-tree/nvim-web-devicons'] = '58447c1fca354bbf184425e4a8d01deecbd6f3c4',
  ['nvim-tree/nvim-tree.lua'] = '478c69c0fe253caea88de9c5e138bfa77395a59a',
  ['akinsho/bufferline.nvim'] = '655133c3b4c3e5e05ec549b9f8cc2894ac6f51b3',
  ['nvim-lualine/lualine.nvim'] = '221ce6b2d999187044529f49da6554a92f740a96',
  ['nvim-lua/plenary.nvim'] = '74b06c6c75e4eeb3108ec01852001636d85a932b',
  ['nvim-telescope/telescope.nvim'] = '40aedd8a68c78a656a10a8d62d80c54af59420fb',
  ['nvim-treesitter/nvim-treesitter'] = 'e6be2ff65d89df5039cad7a422600757bbf81d02',
  ['neovim/nvim-lspconfig'] = '3e8d598d3b5f8338a41699c436e5fa11d2666cf0',
  ['hrsh7th/nvim-cmp'] = '2ffe79f1f021def8dd1fcd81deb16f1bb0d989f3',
  ['hrsh7th/cmp-nvim-lsp'] = 'cbc7b02bb99fae35cb42f514762b89b5126651ef',
  ['hrsh7th/cmp-buffer'] = 'b74fab3656eea9de20a9b8116afa3cfc4ec09657',
  ['lewis6991/gitsigns.nvim'] = '070a5d7b985546cc57e1fc61e5bc507fecac6045',
  ['sindrets/diffview.nvim'] = '4516612fe98ff56ae0415a259ff6361a89419b0a',
  ['folke/trouble.nvim'] = 'bd67efe408d4816e25e8491cc5ad4088e708a69a',
  ['mfussenegger/nvim-dap'] = 'cfa2d58f4537aca6ca83e2de1a0d9f1491121264',
  ['nvim-neotest/nvim-nio'] = 'edcc181a875301dd21840189aa2f2f9ad69fc172',
  ['rcarriga/nvim-dap-ui'] = 'cc9dd33aade7f20bae414d0cba163bc60d4d4b43',
}
local plugins = {
  { 'nvim-tree/nvim-web-devicons', lazy = true },
  { 'nvim-tree/nvim-tree.lua', dependencies = { 'nvim-tree/nvim-web-devicons' },
    opts = { view = { width = 32, relativenumber = false }, renderer = { group_empty = true,
      icons = { show = { file = false, folder = false, folder_arrow = false, git = false } } },
      actions = { open_file = { quit_on_open = false } } } },
  { 'akinsho/bufferline.nvim', dependencies = { 'nvim-tree/nvim-web-devicons' },
    opts = { options = { diagnostics = 'nvim_lsp', separator_style = 'thin',
      show_buffer_icons = false, show_close_icon = false, show_buffer_close_icons = false } } },
  { 'nvim-lualine/lualine.nvim', dependencies = { 'nvim-tree/nvim-web-devicons' } },
  { 'nvim-lua/plenary.nvim', lazy = true },
  { 'nvim-telescope/telescope.nvim', dependencies = { 'nvim-lua/plenary.nvim' } },
  { 'nvim-treesitter/nvim-treesitter', lazy = false, build = ':TSUpdate' },
  { 'neovim/nvim-lspconfig' },
  { 'hrsh7th/nvim-cmp', dependencies = { 'hrsh7th/cmp-nvim-lsp' } },
  { 'hrsh7th/cmp-nvim-lsp' },
  { 'hrsh7th/cmp-buffer' },
  { 'lewis6991/gitsigns.nvim', opts = { signcolumn = true, numhl = false } },
  { 'sindrets/diffview.nvim' },
  { 'folke/trouble.nvim', dependencies = { 'nvim-tree/nvim-web-devicons' }, opts = {} },
  { 'mfussenegger/nvim-dap' },
  { 'nvim-neotest/nvim-nio' },
  { 'rcarriga/nvim-dap-ui', dependencies = { 'mfussenegger/nvim-dap', 'nvim-neotest/nvim-nio' } },
}
for _, plugin in ipairs(plugins) do plugin.commit = assert(pins[plugin[1]]) end
require('lazy').setup(plugins, {
  lockfile = config_dir .. '/lazy-lock.json',
  checker = { enabled = false },
  change_detection = { enabled = false },
  install = { missing = true },
  ui = { border = 'rounded' },
})

local tree_api = require('nvim-tree.api')
require('nvim-tree').setup({
  view = { width = 32 }, renderer = { group_empty = true,
    icons = { show = { file = false, folder = false, folder_arrow = false, git = false } } },
  on_attach = function(bufnr)
    tree_api.config.mappings.default_on_attach(bufnr)
    vim.keymap.set('n', '<LeftMouse>', function()
      local mouse = vim.fn.getmousepos()
      if mouse.winid ~= 0 and mouse.line > 0 and
         vim.api.nvim_win_get_buf(mouse.winid) == bufnr then
        vim.api.nvim_set_current_win(mouse.winid)
        vim.api.nvim_win_set_cursor(mouse.winid, { mouse.line, 0 })
        tree_api.node.open.edit()
      end
    end, { buffer = bufnr, desc = 'Open clicked file or directory' })
  end,
})
-- Herdr can resize the terminal after Neovim opens. Restore the fixed sidebar width.
vim.api.nvim_create_autocmd({ 'VimResized', 'BufWinEnter' }, {
  callback = function()
    vim.schedule(function()
      if vim.o.columns < 72 then return end
      for _, win in ipairs(vim.api.nvim_list_wins()) do
        if vim.bo[vim.api.nvim_win_get_buf(win)].filetype == 'NvimTree'
          and vim.api.nvim_win_get_width(win) < 28 then
          vim.api.nvim_win_set_width(win, 32)
        end
      end
    end)
  end,
})
local function setup_bufferline()
  local highlights = {
    fill = { fg = palette.muted, bg = palette.bg },
    background = { fg = palette.muted, bg = palette.panel },
    buffer_visible = { fg = palette.text, bg = palette.panel },
    buffer_selected = { fg = palette.text, bg = palette.bg, bold = true, italic = true },
    separator = { fg = palette.bg, bg = palette.panel },
    separator_selected = { fg = palette.bg, bg = palette.bg },
    indicator_visible = { fg = palette.muted, bg = palette.panel },
    indicator_selected = { fg = palette.teal, bg = palette.bg },
  }
  require('bufferline').setup({
    options = { diagnostics = 'nvim_lsp', separator_style = 'thin',
      show_buffer_icons = false, show_close_icon = false, show_buffer_close_icons = false },
    highlights = highlights,
  })
  -- Bufferline keeps its existing highlight groups when setup runs again.
  for name, attrs in pairs(highlights) do
    local group = 'BufferLine' .. name:gsub('^%l', string.upper):gsub('_(%l)', string.upper)
    vim.api.nvim_set_hl(0, group, attrs)
  end
end
setup_bufferline()
require('gitsigns').setup()
require('trouble').setup({ keys = { ['<LeftMouse>'] = 'jump' } })
local diff_actions = require('diffview.actions')
require('diffview').setup({ keymaps = {
  file_panel = { { 'n', '<LeftMouse>', diff_actions.select_entry } },
  file_history_panel = { { 'n', '<LeftMouse>', diff_actions.select_entry } },
} })
require('dapui').setup()
local dap = require('dap')
dap.listeners.after.event_initialized.worklane = function() require('dapui').open() end
dap.listeners.before.event_terminated.worklane = function() require('dapui').close() end
dap.listeners.before.event_exited.worklane = function() require('dapui').close() end

local lsp_capabilities = require('cmp_nvim_lsp').default_capabilities()
local cmp = require('cmp')
cmp.setup({
  mapping = cmp.mapping.preset.insert({
    ['<C-Space>'] = cmp.mapping.complete(),
    ['<CR>'] = cmp.mapping.confirm({ select = false }),
    ['<Tab>'] = cmp.mapping.select_next_item(),
    ['<S-Tab>'] = cmp.mapping.select_prev_item(),
  }),
  sources = cmp.config.sources({ { name = 'nvim_lsp' } }, { { name = 'buffer' } }),
})

-- Activate installed language servers only. Worklane never downloads servers silently.
for name, command in pairs({
  rust_analyzer = 'rust-analyzer', pyright = 'pyright-langserver',
  ts_ls = 'typescript-language-server', bashls = 'bash-language-server',
  lua_ls = 'lua-language-server', gopls = 'gopls', jdtls = 'jdtls',
}) do
  if vim.fn.executable(command) == 1 then
    vim.lsp.config(name, { capabilities = lsp_capabilities })
    vim.lsp.enable(name)
  end
end

vim.api.nvim_create_autocmd('FileType', {
  callback = function() pcall(vim.treesitter.start) end,
})

local ok_ts, treesitter = pcall(require, 'nvim-treesitter')
if ok_ts and vim.env.WORKLANE_IDE_BOOTSTRAP == '1' then
  treesitter.install({ 'bash', 'c', 'css', 'go', 'html', 'javascript', 'json', 'lua',
    'markdown', 'python', 'rust', 'toml', 'tsx', 'typescript', 'vim', 'yaml' }):wait(300000)
end

local telescope = require('telescope.builtin')
vim.keymap.set('n', '<leader>e', '<cmd>NvimTreeToggle<cr>', { desc = 'File tree' })
vim.keymap.set('n', '<leader>f', telescope.find_files, { desc = 'Find files' })
vim.keymap.set('n', '<leader>g', telescope.live_grep, { desc = 'Search project' })
vim.keymap.set('n', '<leader>b', telescope.buffers, { desc = 'Buffers' })
vim.keymap.set('n', '<leader>d', '<cmd>Trouble diagnostics toggle<cr>', { desc = 'Diagnostics' })
vim.keymap.set('n', '<leader>v', '<cmd>DiffviewOpen<cr>', { desc = 'Git diff' })
vim.keymap.set('n', '<leader>t', '<cmd>terminal<cr>', { desc = 'Terminal' })
vim.keymap.set('n', '<F5>', dap.continue, { desc = 'Debug continue' })
vim.keymap.set('n', '<F9>', dap.toggle_breakpoint, { desc = 'Toggle breakpoint' })
vim.keymap.set('n', '<F10>', dap.step_over, { desc = 'Debug step over' })
vim.keymap.set('n', '<F11>', dap.step_into, { desc = 'Debug step into' })

local section = function(text, callback)
  local name = 'WorklaneClick' .. text:gsub('[^%w]', '')
  _G[name] = function() callback() end
  return { function() return text end, on_click = function() _G[name]() end }
end
local function setup_lualine()
require('lualine').setup({
  options = { icons_enabled = false,
    theme = { normal = { a = { fg = palette.bg, bg = palette.teal, gui = 'bold' },
    b = { fg = palette.text, bg = palette.panel }, c = { fg = palette.text, bg = palette.bg } },
    insert = { a = { fg = palette.bg, bg = palette.green } },
    visual = { a = { fg = palette.bg, bg = palette.violet } },
    replace = { a = { fg = palette.bg, bg = palette.red } } },
    section_separators = { left = ' ', right = ' ' }, component_separators = '│' },
  sections = {
    lualine_a = { 'mode' }, lualine_b = { 'branch', 'diff' },
    lualine_c = { 'filename', section('Files', function() vim.cmd('NvimTreeToggle') end),
      section('Find', telescope.find_files), section('Git', function() vim.cmd('DiffviewOpen') end) },
    lualine_x = { 'diagnostics',
      section('Debug', function() require('dapui').toggle() end),
      section('▶', dap.continue), section('●', dap.toggle_breakpoint),
      section('↷', dap.step_over), section('■', dap.terminate) },
    lualine_y = { 'filetype' }, lualine_z = { 'location' },
  },
})
end
setup_lualine()

function _G.WorklaneApplyPalette()
  palette = dofile(config_dir .. '/palette.lua')
  apply_highlights()
  setup_bufferline()
  setup_lualine()
  require('lualine').refresh()
  vim.cmd('redraw!')
  return 'applied'
end

function _G.WorklaneDiagnostics()
  local rows = {}
  for _, item in ipairs(vim.diagnostic.get(nil)) do
    rows[#rows + 1] = { file = vim.api.nvim_buf_get_name(item.bufnr),
      line = item.lnum + 1, column = item.col + 1,
      severity = item.severity, message = item.message }
  end
  return vim.json.encode(rows)
end

function _G.WorklaneDebug(name)
  local adapter_file = vim.fn.getcwd() .. '/.worklane/ide.toml'
  if vim.fn.filereadable(adapter_file) == 1 then
    local result = vim.system({ 'python3', '-c',
      'import json,sys,tomllib; print(json.dumps(tomllib.load(open(sys.argv[1], "rb")).get("debug", {}).get("adapters", {})))',
      adapter_file }, { text = true }):wait()
    if result.code ~= 0 then return 'Invalid debug adapters in .worklane/ide.toml' end
    for kind, adapter in pairs(vim.json.decode(result.stdout)) do
      dap.adapters[kind] = adapter
    end
  end
  if dap.adapters.python == nil and vim.fn.executable('python3') == 1 then
    local probe = vim.system({ 'python3', '-c', 'import debugpy' }):wait()
    if probe.code == 0 then
      dap.adapters.python = { type = 'executable', command = 'python3',
        args = { '-m', 'debugpy.adapter' } }
    end
  end
  local launch = vim.fn.getcwd() .. '/.vscode/launch.json'
  if vim.fn.filereadable(launch) == 1 then pcall(dap.ext.vscode.load_launchjs, launch) end
  for _, configs in pairs(dap.configurations) do
    for _, config in ipairs(configs) do
      if config.name == name then dap.run(config); return 'started' end
    end
  end
  return 'No debug configuration named ' .. name
end

vim.api.nvim_create_user_command('WorklaneTask', function(opts)
  vim.fn.jobstart({ vim.fn.expand('~/.local/bin/worklane-ide'), 'task', opts.args }, { detach = true })
end, { nargs = 1, complete = 'file' })
vim.api.nvim_create_user_command('WorklaneDebug', function(opts)
  print(_G.WorklaneDebug(opts.args))
end, { nargs = 1 })

vim.api.nvim_create_autocmd('VimEnter', {
  once = true,
  callback = function()
    if vim.fn.argc() == 1 and vim.fn.isdirectory(vim.fn.argv(0)) == 1 then
      vim.cmd('NvimTreeOpen')
      telescope.find_files()
    end
  end,
})
