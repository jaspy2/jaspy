using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using Jaspy.Switchmaster.Attributes;
using Jaspy.Switchmaster.Data;
using Jaspy.Switchmaster.Data.Entities;
using Jaspy.Switchmaster.Data.Models;
using Jaspy.Switchmaster.Hubs;
using Microsoft.AspNetCore.Mvc;
using Microsoft.AspNetCore.Mvc.Routing;
using Microsoft.AspNetCore.SignalR;
using Microsoft.EntityFrameworkCore;
using Microsoft.Extensions.Logging;

namespace Jaspy.Switchmaster.Controllers
{
    [ApiController]
    [Route("api/[controller]")]
    public class SwitchController : ControllerBase
    {
        private readonly ILogger<SwitchController> _logger;
        private readonly SwitchmasterDbContext _dbContext;
        private readonly NexusClient _nexusClient;
        private readonly IHubContext<SwitchHub> _hubContext;

        public SwitchController(ILoggerFactory loggerFactory, SwitchmasterDbContext dbContext, NexusClient nexusClient, IHubContext<SwitchHub> hubContext)
        {
            _logger = loggerFactory.CreateLogger<SwitchController>();
            _dbContext = dbContext;
            _nexusClient = nexusClient;
            _hubContext = hubContext;
        }

        #region Accessors

        [HttpGet]
        public async Task<IActionResult> List([FromQuery] ListFilter filter = null)
        {
            var query = _dbContext.Switches as IQueryable<Switch>;

            if (filter != null)
            {
                if (!string.IsNullOrEmpty(filter.Contains))
                {
                    query = query.Where(t => t.Fqdn.ToLower().Contains(filter.Contains));
                }

                if (filter.Limit > 0)
                {
                    query = query.Skip(filter.Page * filter.Limit).Take(filter.Limit);
                }
            }

            return Ok(ToViewModel(await query.ToListAsync()));
        }

        [HttpGet("{fqdn}")]
        public async Task<IActionResult> Find(string fqdn)
        {
            var match = await _dbContext.Switches.FindAsync(fqdn);
            if (match == null)
            {
                return NotFound();
            }

            return Ok(ToViewModel(match));
        }

        #endregion

        #region Mutators

        [HttpPatch("{fqdn}")]
        public async Task<IActionResult> Patch([FromRoute] string fqdn, [FromBody] SwitchViewModel model)
        {
            if (!ModelState.IsValid)
            {
                return BadRequest(ModelState);
            }

            var match = await _dbContext.Switches.FindAsync(fqdn);
            if (match == null)
            {
                return NotFound();
            }

            match.DeployState = model.DeployState;
            match.Configured = model.Configured;
            await _dbContext.SaveChangesAsync();

            await _hubContext.Clients.All.SendAsync("Update", model);

            return Ok();
        }

        [HttpDelete("{fqdn}")]
        public async Task<IActionResult> Delete(string fqdn)
        {
            var match = await _dbContext.Switches.FindAsync(fqdn);
            if (match == null)
            {
                return Ok();
            }

            _dbContext.Switches.Remove(match);
            await _dbContext.SaveChangesAsync();

            await _hubContext.Clients.All.SendAsync("Delete", fqdn);

            return Ok(ToViewModel(match));
        }

        [HttpSynchronize("synchronize")]
        public async Task<IActionResult> Synchronize()
        {
            await _hubContext.Clients.All.SendAsync("StartingSynchronization");

            try
            {
                var allSwitches = await _nexusClient.ListDevicesAsync();

                var added = 0;
                var existing = 0;
                var newSwitches = new List<SwitchViewModel>();
                foreach (var entry in allSwitches)
                {
                    var fqdn = $"{entry.Name}.{entry.DnsDomain}";
                    var match = await _dbContext.Switches.FindAsync(fqdn);
                    if (match == null)
                    {
                        var newSwitch = new Switch
                        {
                            Fqdn = fqdn,
                            Configured = true,
                            DeployState = DeployState.Stationed
                        };
                        var result = await _dbContext.AddAsync(newSwitch);
                        added++;
                        newSwitches.Add(ToViewModel(result.Entity));
                    }
                    else
                    {
                        existing++;
                    }
                }

                await _dbContext.SaveChangesAsync();

                var synchronizationResult = new SynchronizationResult
                {
                    Added = added,
                    Existing = existing,
                    NewSwitches = newSwitches
                };

                await _hubContext.Clients.All.SendAsync("Synchronize", synchronizationResult);

                return Ok(synchronizationResult);
            }
            catch (Exception ex)
            {
                _logger.LogError(ex, "Failed to synchronize switches.");
                await _hubContext.Clients.All.SendAsync("Synchronize", null);
                throw;
            }
        }

        #endregion

        #region Helpers

        private SwitchViewModel ToViewModel(Switch entity) => new SwitchViewModel
        {
            Fqdn = entity.Fqdn,
            DeployState = entity.DeployState,
            Configured = entity.Configured
        };

        private IEnumerable<SwitchViewModel> ToViewModel(IEnumerable<Switch> entities) =>
            entities.Select(ToViewModel);

        #endregion
    }
}