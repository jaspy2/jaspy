<template>
    <div>
        switch!
    </div>
</template>

<script lang="ts">
  import { Component, Prop, Vue } from 'vue-property-decorator';
  import { Switch } from "@/store/modules/switch";

  @Component
  export default class SwitchStatePanel extends Vue {
    @Prop() private switch!: Switch;
  }
</script>

<style scoped lang="scss">

</style>