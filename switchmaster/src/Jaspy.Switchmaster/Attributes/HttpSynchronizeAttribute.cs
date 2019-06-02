using System;
using System.Collections.Generic;
using Microsoft.AspNetCore.Mvc.Routing;

namespace Jaspy.Switchmaster.Attributes
{
    public class HttpSynchronizeAttribute : HttpMethodAttribute
    {
        private static readonly IEnumerable<string> SupportedMethods = new string[1]
        {
            "SYNCHRONIZE"
        };

        public HttpSynchronizeAttribute()
            : base(SupportedMethods)
        {
        }

        public HttpSynchronizeAttribute(string template)
            : base(SupportedMethods, template)
        {
            if (template == null)
                throw new ArgumentNullException(nameof (template));
        }
    }
}