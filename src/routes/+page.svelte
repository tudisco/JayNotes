<script lang="ts">
  import Sidebar from "$lib/components/Sidebar.svelte";
  import EditorPane from "$lib/components/EditorPane.svelte";
</script>

<div class="app-shell">
  <Sidebar />
  <EditorPane />
</div>

<style>
  .app-shell {
    display: flex;
    width: 100vw;
    height: 100vh;
    overflow: hidden;
  }
</style>
