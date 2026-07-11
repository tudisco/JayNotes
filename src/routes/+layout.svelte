<script lang="ts">
  import "../app.css";
  // Milkdown Crepe styles + our token remap (loaded after app.css so the
  // token overrides win over Crepe's frame defaults).
  import "$lib/styles/editor.css";
  // Importing the theme store here ensures it initializes (persists + applies).
  import "$lib/stores/theme";

  let { children } = $props();
</script>

{@render children()}
