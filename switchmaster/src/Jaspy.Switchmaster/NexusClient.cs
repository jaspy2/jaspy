using System;
using System.Collections.Generic;
using System.Net.Http;
using System.Net.Http.Headers;
using System.Text;
using System.Threading.Tasks;
using Jaspy.Switchmaster.Data.Models;
using Microsoft.Extensions.DependencyInjection;
using Newtonsoft.Json;

namespace Jaspy.Switchmaster
{
    public class NexusConfiguration
    {
        public string ApiRoot { get; set; }
        public string Username { get; set; }
        public string Password { get; set; }
    }
    
    public class NexusDevice
    {
        public int Id { get; set; }
        public string Name { get; set; }
        public string DnsDomain { get; set; }
        public string SnmpCommunity { get; set; }
        public string BaseMac { get; set; }
        public bool? PollingEnabled { get; set; }
        public string OsInfo { get; set; }
    }
    
    public class NexusClient : IDisposable
    {
        private readonly string _apiRoot;
        private readonly HttpClient _client;

        public NexusClient(NexusConfiguration config)
        {
            _apiRoot = config.ApiRoot;
            _client = new HttpClient();
            var encodedCredentials = Convert.ToBase64String(Encoding.ASCII.GetBytes($"{config.Username}:{config.Password}"));
            _client.DefaultRequestHeaders.Authorization = new AuthenticationHeaderValue("Basic", encodedCredentials);
        }

        public async Task<IEnumerable<NexusDevice>> ListDevicesAsync()
        {
            var endpointUrl = $"{_apiRoot}/device";
            var response = await _client.GetStringAsync(endpointUrl);
            return JsonConvert.DeserializeObject<IEnumerable<NexusDevice>>(response);
        }

        public void Dispose()
        {
            _client?.Dispose();
        }
    }
}