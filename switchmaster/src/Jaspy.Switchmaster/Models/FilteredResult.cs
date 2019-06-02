using System.Collections;
using System.Collections.Generic;

namespace Jaspy.Switchmaster.Data.Models
{
    public class FilteredResult<T>
    {
        public long AmountFiltered { get; set; }
        public IEnumerable<T> Result { get; set; }
    }
}