<template>
    <div class="state-panel elevation-2">
        <div class="fqdn" title="Device FQDN">{{ fqdn }}</div>
        <div class="spacer"></div>
        <div class="state" title="Deployment state">
            <v-select
                label="Select State"
                v-model="deployState"
                :items="deployStates"
                item-text="label"
                item-value="value"
                flat solo
                hide-details></v-select>
        </div>
        <div class="transition" title="Transition to next state">
            <button>
                <v-icon>chevron_right</v-icon>
            </button>
        </div>
        <div class="configured" :class="{ configured: isConfigured }" title="Configured">
            <v-icon>settings</v-icon>
        </div>
        <div class="active-ports" :class="{ active: activePorts > 0 }" title="Connected interfaces">
            <v-icon>device_hub</v-icon>
            {{ activePorts }}
        </div>
    </div>
</template>

<script lang="ts">
  import { Component, Prop, Vue } from 'vue-property-decorator';
  import { DeployState, Switch } from '@/store/modules/switch';

  @Component
  export default class SwitchStatePanel extends Vue {
    @Prop(String)
    public readonly fqdn!: string;
    @Prop(Number)
    public deployState!: number;
    @Prop(Boolean)
    public readonly isConfigured?: boolean;
    @Prop(Number)
    public readonly activePorts!: number;

    get configured(): string {
      return this.isConfigured ? 'Configured' : 'Unconfigured';
    }

    get state(): string {
      switch (this.deployState) {
        case DeployState.Stationed:
          return 'Stationed';
        default:
          return 'Unknown';
      }
    }

    get deployStates(): Array<{label: string, value: number}> {
      const result = [] as Array<{label: string, value: number}>;
      for (const key in DeployState) {
        if (typeof(DeployState[key]) === 'number') {
          result.push({
            label: key,
            value: DeployState[key] as any as number,
          });
        }
      }
      return result;
    }
  }
</script>

<style scoped lang="scss">
.state-panel {
    display: flex;
    flex-direction: row;
    align-items: center;
    background-color: #424242;
    color: rgba(255, 255, 255, 0.9);
    
    .fqdn {
        padding: 10px 15px;
    }
    
    .spacer {
        flex: 1;
    }
    
    .state {
        width: 150px;
    }
    
    .transition {
        button {
            padding: 10px 15px;
            background-color: rgba(255, 255, 255, 0.1);
        }
    }
    
    .configured {
        padding: 10px 15px;
        background-color: #FF5252;

        &.configured {
            background-color: #4CAF50;
        }
    }
    
    .active-ports {
        padding: 10px 15px;
        font-size: 1.3em;
        background-color: #FF5252;
        
        &.active {
            background-color: #4CAF50;
        }
    }
}
</style>
