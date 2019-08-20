using System.Collections;
using System.Collections.Generic;

namespace Jaspy.Switchmaster.Data.Models
{
    public class SwitchViewModel
    {
        public string Fqdn { get; set; }
        public bool? Configured { get; set; }
        public DeployState? DeployState { get; set; }
    }
    
    public class SynchronizationResult
    {
        public IEnumerable<SwitchViewModel> NewSwitches { get; set; }
        public long Added { get; set; }
        public long Existing { get; set; }
    }
}