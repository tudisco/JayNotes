<script lang="ts">
  import "../app.css";
  // Importing the theme store here ensures it initializes (persists + applies).
  import "$lib/stores/theme";

  let { children } = $props();
</script>

{@render children()}
