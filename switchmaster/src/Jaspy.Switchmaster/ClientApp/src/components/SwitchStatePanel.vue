<template>
    <v-flex fill-height elevation-2>
        <span>{{ fqdn }}</span>
        <span>{{ deployState }}</span>
        <span>{{ configured }}</span>
        <span>{{ activePorts }}</span>
    </v-flex>
</template>

<script lang="ts">
  import { Component, Prop, Vue } from 'vue-property-decorator';
  import { DeployState, Switch } from '@/store/modules/switch';

  @Component
  export default class SwitchStatePanel extends Vue {
    @Prop(String)
    public readonly fqdn!: string;
    @Prop(Number)
    public readonly deployState!: DeployState;
    @Prop(Boolean)
    public readonly configured?: boolean;
    @Prop(Number)
    public readonly activePorts!: number;
  }
</script>

<style scoped lang="scss">

</style>