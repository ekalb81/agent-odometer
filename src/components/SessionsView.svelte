<script lang="ts">
  import { sessions } from '../lib/stores/sessions';
</script>

<div class="flex flex-col items-center justify-center h-full gap-4 text-slate-400">
  <div class="text-6xl">📋</div>
  <p class="text-lg font-medium text-slate-300">
    {$sessions.size} sessions
  </p>
  <p class="text-sm">Phase 3 will populate this view with the sessions table.</p>
</div>
