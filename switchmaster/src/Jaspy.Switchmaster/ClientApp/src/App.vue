<template>
  <v-app dark>
      <v-container fluid grid-list-xl>
          <v-layout row wrap>
              <v-flex v-for="item in switches">
                  <SwitchStatePanel :fqdn="item.fqdn"
                                    :deployState="item.deployState"
                                    :isConfigured="item.configured"
                                    :activePorts="0"
                                    :update-item="update"></SwitchStatePanel>
              </v-flex>
              <v-flex xs12>
                  <v-btn color="warning" flat @click="synchronize">Synchronize</v-btn>
              </v-flex>
          </v-layout>
      </v-container>
      <StatusOverlay :visible="processing"></StatusOverlay>
  </v-app>
</template>

<script lang="ts">
import { mapActions, mapState } from 'vuex';
import { Component, Vue } from 'vue-property-decorator';
import SwitchStatePanel from '@/components/SwitchStatePanel.vue';
import StatusOverlay from '@/components/StatusOverlay.vue';
import { HubConnection, HubConnectionBuilder } from '@aspnet/signalr';
import { Switch, SynchronizationResult } from '@/store/models';

let hubConnection: HubConnection | undefined;

@Component({
  components: {
    StatusOverlay,
    SwitchStatePanel
  },
  async beforeCreate() {
    hubConnection = new HubConnectionBuilder()
      .withUrl('hubs/switch')
      .build();
    hubConnection.on('Update', (updatedSwitch: Switch) => {
      this.$store.commit('updateItem', updatedSwitch);
    });
    hubConnection.on('StartingSynchronization', () => {
      this.$store.commit('setProcessing', true);
    });
    hubConnection.on('Synchronize', (result: SynchronizationResult) => {
      if (result !== null) {
        this.$store.commit('setSyncResult', result);
      }
      this.$store.commit('setProcessing', false);
    });
    await hubConnection.start();
    await this.$store.dispatch('fetch');
  },
  async beforeDestroy() {
    if (hubConnection) {
      await hubConnection.stop();
    }
    hubConnection = undefined;
  },
  computed: {
    ...mapState({
      switches: (state: any) => state.items,
      processing: (state: any) => state.processing
    })
  },
  methods: {
    ...mapActions([
      'synchronize',
      'update'
    ])
  }
})
export default class App extends Vue {}
</script>

<style lang="scss">
body {
    background-color: #303030;
}

#app {
  font-family: 'Roboto', Helvetica, Arial, sans-serif;
  text-align: center;
  color: #2c3e50;
}
</style>
