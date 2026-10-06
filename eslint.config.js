const js = require('@eslint/js');

module.exports = [
  { ignores: ['node_modules/**', 'out/**', 'dist/**', 'resources/**', 'src/renderer/**', 'electron.vite.config.js', 'eslint.config.js'] },
  js.configs.recommended,
  {
    files: ['src/main/**/*.js', 'src/preload/**/*.js', 'scripts/**/*.js', 'tests/**/*.js'],
    languageOptions: {
      ecmaVersion: 2022,
      sourceType: 'commonjs',
      globals: {
        require: 'readonly', module: 'writable', process: 'readonly', __dirname: 'readonly',
        Buffer: 'readonly', console: 'readonly', setTimeout: 'readonly', clearTimeout: 'readonly',
        setInterval: 'readonly', clearInterval: 'readonly', URL: 'readonly', setImmediate: 'readonly'
      }
    },
    rules: { 'no-empty': ['error', { allowEmptyCatch: true }], 'no-unused-vars': ['error', { argsIgnorePattern: '^_' }] }
  }
];
