<!-- The RustCloud logo (three gears); the source image is docs/logo-source.png. -->
<script lang="ts">
  import logo from '../assets/logo.png';

  let { size = 26 }: { size?: number } = $props();
</script>

<img src={logo} alt="" height={size} style:height="{size}px" style:width="auto" />
